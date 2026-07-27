//! Encode a Python message instance into protobuf wire bytes.
//!
//! The encoder walks a [`Descriptor`], reads each field off the instance with
//! an interned-name `getattr`, and appends the tag + payload to the output
//! buffer. Nested messages encode in place: the length slot is reserved with
//! [`wire::begin_len_prefix`] and patched afterwards, so no temporary buffers
//! are allocated per submessage. Nested messages recurse through the child's
//! own `__fastproto__` descriptor, so no pre-resolved class references are
//! needed here.

use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyList, PyString, PyTuple};

use crate::descriptor::{FieldKind, Label, MapValue, ScalarType, MAX_DEPTH};
use crate::message::{Descriptor, LinkedRef, Refs};
use crate::wellknown;
use crate::wire;

/// Encode `instance` according to `desc`, appending to `buf`.
///
/// `depth` is the current message-nesting level (0 at the top); it is bounded
/// by [`MAX_DEPTH`], which also catches reference cycles between Python
/// objects (a self-referential message would otherwise recurse forever).
pub fn encode_message(
    py: Python<'_>,
    instance: &Bound<'_, PyAny>,
    desc: &Descriptor,
    buf: &mut Vec<u8>,
    depth: usize,
) -> PyResult<()> {
    if depth > MAX_DEPTH {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "message nesting exceeded {MAX_DEPTH} levels"
        )));
    }
    let refs = desc.refs();

    // Per-group counters for the at-most-one-oneof-member check, folded into
    // the main walk so members aren't read twice. Checked after the loop; on
    // violation the whole (local) buffer is discarded, so nothing partial is
    // observable. Small messages count on the stack.
    let n_oneofs = desc.inner.oneofs.len();
    let mut oneofs_stack = [0u8; 16];
    let mut oneofs_heap;
    let oneof_counts: &mut [u8] = if n_oneofs <= 16 {
        &mut oneofs_stack[..n_oneofs]
    } else {
        oneofs_heap = vec![0u8; n_oneofs];
        &mut oneofs_heap
    };

    for (idx, field) in desc.inner.fields.iter().enumerate() {
        let value = instance.getattr(desc.field_names[idx].bind(py))?;

        if let FieldKind::Map {
            key,
            value: val_kind,
        } = &field.kind
        {
            encode_map(py, buf, field.number, *key, val_kind, &value, refs, depth)?;
            continue;
        }

        match field.label {
            Label::Repeated => {
                encode_repeated(py, buf, field.number, &field.kind, &value, refs, depth)?;
            }
            Label::Optional => {
                if !value.is_none() {
                    if let Some(group) = field.oneof_index {
                        oneof_counts[group as usize] =
                            oneof_counts[group as usize].saturating_add(1);
                    }
                    encode_single(py, buf, field.number, &field.kind, &value, refs, depth)?;
                }
            }
            Label::Single => match &field.kind {
                FieldKind::Scalar(scalar) => {
                    encode_scalar_field(buf, field.number, *scalar, &value, true)?;
                }
                FieldKind::Enum => {
                    let v: i32 = value.extract()?;
                    if v != 0 {
                        wire::write_tag(buf, field.number, wire::WireType::Varint);
                        wire::write_varint(buf, v as i64 as u64);
                    }
                }
                // Message-like fields are always Optional; maps are handled above.
                _ => encode_single(py, buf, field.number, &field.kind, &value, refs, depth)?,
            },
        }
    }

    for (group, count) in oneof_counts.iter().enumerate() {
        if *count > 1 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "at most one member of oneof '{}' may be set, got {}",
                desc.inner.oneofs[group], count
            )));
        }
    }

    // Re-emit unknown fields captured by the decoder (stored on the hidden
    // `Message` slot). The slot is unset on instances that predate the
    // base-class `__post_init__`, so an AttributeError just means "nothing to
    // preserve".
    if let Ok(raw) = instance.getattr(pyo3::intern!(py, "_fastproto_unknown")) {
        buf.extend_from_slice(byte_slice(&raw)?);
    }
    Ok(())
}

/// Borrow a `bytes`/`bytearray` value as `&[u8]` without per-element boxing.
///
/// `value.extract::<Vec<u8>>()` goes through pyo3's generic sequence path,
/// which allocates a Python int per byte -- catastrophic for large payloads.
/// Reading the buffer directly is a plain memory borrow. The caller must copy
/// out before returning to Python (no arbitrary Python runs in between here).
fn byte_slice<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<&'a [u8]> {
    if let Ok(b) = value.downcast::<PyBytes>() {
        return Ok(b.as_bytes());
    }
    if let Ok(ba) = value.downcast::<PyByteArray>() {
        // SAFETY: the returned slice is consumed synchronously by the caller
        // (copied into the output buffer) with no intervening Python execution
        // that could resize or free the bytearray.
        return Ok(unsafe { ba.as_bytes() });
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "bytes field requires a bytes or bytearray value",
    ))
}

/// Write a single tagged value (scalar, enum, or message).
fn encode_single(
    py: Python<'_>,
    buf: &mut Vec<u8>,
    number: u32,
    kind: &FieldKind,
    value: &Bound<'_, PyAny>,
    refs: Option<&Refs>,
    depth: usize,
) -> PyResult<()> {
    match kind {
        FieldKind::Scalar(scalar) => {
            encode_scalar_field(buf, number, *scalar, value, false)?;
        }
        FieldKind::Enum => {
            let v: i32 = value.extract()?;
            wire::write_tag(buf, number, wire::WireType::Varint);
            wire::write_varint(buf, v as i64 as u64);
        }
        FieldKind::Message => {
            wire::write_tag(buf, number, wire::WireType::Len);
            let len_pos = wire::begin_len_prefix(buf);
            encode_nested(py, value, refs.and_then(|r| r.get(&number)), buf, depth)?;
            wire::finish_len_prefix(buf, len_pos);
        }
        FieldKind::Timestamp => {
            let (secs, nanos) = wellknown::datetime_to_parts(py, value)?;
            write_parts_field(buf, number, secs, nanos);
        }
        FieldKind::Duration => {
            let (secs, nanos) = wellknown::timedelta_to_parts(value)?;
            write_parts_field(buf, number, secs, nanos);
        }
        FieldKind::Map { .. } => unreachable!("maps handled separately"),
    }
    Ok(())
}

/// Write a Timestamp/Duration submessage as one tagged length-delimited field.
fn write_parts_field(buf: &mut Vec<u8>, number: u32, secs: i64, nanos: i32) {
    wire::write_tag(buf, number, wire::WireType::Len);
    let len_pos = wire::begin_len_prefix(buf);
    wellknown::encode_parts(buf, secs, nanos);
    wire::finish_len_prefix(buf, len_pos);
}

/// Encode a nested message's payload (no tag/length).
///
/// When the field was linked and the value is exactly the linked class (one
/// pointer compare), the descriptor cached at link time is used; otherwise the
/// descriptor is read off the value's own type, so duck-typed values and
/// unlinked descriptors keep working.
fn encode_nested(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    linked: Option<&LinkedRef>,
    buf: &mut Vec<u8>,
    depth: usize,
) -> PyResult<()> {
    if let Some(linked) = linked {
        if let Some(desc) = &linked.desc {
            if value.get_type().is(linked.class.bind(py)) {
                let desc_ref = desc.bind(py).borrow();
                return encode_message(py, value, &desc_ref, buf, depth + 1);
            }
        }
    }
    let handle = value
        .get_type()
        .getattr(pyo3::intern!(py, "__fastproto__"))?;
    let desc = handle.downcast_into::<Descriptor>().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err("nested value is not a fastproto message")
    })?;
    let desc_ref = desc.borrow();
    encode_message(py, value, &desc_ref, buf, depth + 1)
}

/// Run `f` over the elements of a repeated-field value: an indexed fast path
/// for the common `list`/`tuple` cases, the generic iteration protocol
/// otherwise.
fn for_each_item<'py>(
    value: &Bound<'py, PyAny>,
    mut f: impl FnMut(Bound<'py, PyAny>) -> PyResult<()>,
) -> PyResult<()> {
    if let Ok(list) = value.downcast::<PyList>() {
        for item in list.iter() {
            f(item)?;
        }
        return Ok(());
    }
    if let Ok(tuple) = value.downcast::<PyTuple>() {
        for item in tuple.iter() {
            f(item)?;
        }
        return Ok(());
    }
    for item in value.try_iter()? {
        f(item?)?;
    }
    Ok(())
}

/// Encode a repeated field (packed for numeric scalars/enums, otherwise one
/// tagged entry per element).
fn encode_repeated(
    py: Python<'_>,
    buf: &mut Vec<u8>,
    number: u32,
    kind: &FieldKind,
    value: &Bound<'_, PyAny>,
    refs: Option<&Refs>,
    depth: usize,
) -> PyResult<()> {
    // `str`/`bytes` are iterable, so without this guard `tags="abc"` would
    // silently encode as three one-char entries. A repeated field must be a
    // genuine sequence of elements.
    if value.is_instance_of::<PyString>() || value.is_instance_of::<PyBytes>() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "repeated field must be a list of elements, not a str/bytes",
        ));
    }
    match kind {
        FieldKind::Scalar(scalar) if scalar.is_packable() => {
            // Packed: one Len field holding the concatenated payloads. The tag
            // is rolled back if the sequence turns out to be empty.
            let tag_start = buf.len();
            wire::write_tag(buf, number, wire::WireType::Len);
            let len_pos = wire::begin_len_prefix(buf);
            for_each_item(value, |item| encode_scalar(buf, *scalar, &item))?;
            if buf.len() == len_pos + 1 {
                buf.truncate(tag_start);
            } else {
                wire::finish_len_prefix(buf, len_pos);
            }
        }
        FieldKind::Scalar(scalar) => {
            // string / bytes: one length-delimited entry each.
            for_each_item(value, |item| {
                wire::write_tag(buf, number, scalar.wire_type());
                encode_scalar(buf, *scalar, &item)
            })?;
        }
        FieldKind::Enum => {
            let tag_start = buf.len();
            wire::write_tag(buf, number, wire::WireType::Len);
            let len_pos = wire::begin_len_prefix(buf);
            for_each_item(value, |item| {
                let v: i32 = item.extract()?;
                wire::write_varint(buf, v as i64 as u64);
                Ok(())
            })?;
            if buf.len() == len_pos + 1 {
                buf.truncate(tag_start);
            } else {
                wire::finish_len_prefix(buf, len_pos);
            }
        }
        FieldKind::Message => {
            let linked = refs.and_then(|r| r.get(&number));
            for_each_item(value, |item| {
                wire::write_tag(buf, number, wire::WireType::Len);
                let len_pos = wire::begin_len_prefix(buf);
                encode_nested(py, &item, linked, buf, depth)?;
                wire::finish_len_prefix(buf, len_pos);
                Ok(())
            })?;
        }
        FieldKind::Timestamp => {
            for_each_item(value, |item| {
                let (secs, nanos) = wellknown::datetime_to_parts(py, &item)?;
                write_parts_field(buf, number, secs, nanos);
                Ok(())
            })?;
        }
        FieldKind::Duration => {
            for_each_item(value, |item| {
                let (secs, nanos) = wellknown::timedelta_to_parts(&item)?;
                write_parts_field(buf, number, secs, nanos);
                Ok(())
            })?;
        }
        FieldKind::Map { .. } => unreachable!("maps handled separately"),
    }
    Ok(())
}

/// Encode a `map<K, V>` field: one length-delimited entry message per pair.
#[allow(clippy::too_many_arguments)]
fn encode_map(
    py: Python<'_>,
    buf: &mut Vec<u8>,
    number: u32,
    key: ScalarType,
    value_kind: &MapValue,
    value: &Bound<'_, PyAny>,
    refs: Option<&Refs>,
    depth: usize,
) -> PyResult<()> {
    let dict = value
        .downcast::<PyDict>()
        .map_err(|_| pyo3::exceptions::PyTypeError::new_err("map field must be a dict"))?;
    let linked = refs.and_then(|r| r.get(&number));
    for (k, v) in dict.iter() {
        wire::write_tag(buf, number, wire::WireType::Len);
        let entry_pos = wire::begin_len_prefix(buf);
        // key = field 1
        wire::write_tag(buf, 1, key.wire_type());
        encode_scalar(buf, key, &k)?;
        // value = field 2
        match value_kind {
            MapValue::Scalar(scalar) => {
                wire::write_tag(buf, 2, scalar.wire_type());
                encode_scalar(buf, *scalar, &v)?;
            }
            MapValue::Enum => {
                wire::write_tag(buf, 2, wire::WireType::Varint);
                let iv: i32 = v.extract()?;
                wire::write_varint(buf, iv as i64 as u64);
            }
            MapValue::Message => {
                wire::write_tag(buf, 2, wire::WireType::Len);
                let len_pos = wire::begin_len_prefix(buf);
                encode_nested(py, &v, linked, buf, depth)?;
                wire::finish_len_prefix(buf, len_pos);
            }
            MapValue::Timestamp => {
                let (secs, nanos) = wellknown::datetime_to_parts(py, &v)?;
                write_parts_field(buf, 2, secs, nanos);
            }
            MapValue::Duration => {
                let (secs, nanos) = wellknown::timedelta_to_parts(&v)?;
                write_parts_field(buf, 2, secs, nanos);
            }
        }
        wire::finish_len_prefix(buf, entry_pos);
    }
    Ok(())
}

/// Write one tagged scalar field, extracting the Python value exactly once.
///
/// With `skip_default` (proto3 implicit presence), a value equal to its type
/// default writes nothing. Floats compare by bits, not value: `-0.0 == 0.0`
/// is true, but `-0.0` is not the proto default and must be emitted (google
/// keeps its sign).
fn encode_scalar_field(
    buf: &mut Vec<u8>,
    number: u32,
    scalar: ScalarType,
    value: &Bound<'_, PyAny>,
    skip_default: bool,
) -> PyResult<()> {
    macro_rules! varint_arm {
        ($ty:ty, $to_u64:expr) => {{
            let v: $ty = value.extract()?;
            if !(skip_default && v == 0) {
                wire::write_tag(buf, number, wire::WireType::Varint);
                wire::write_varint(buf, $to_u64(v));
            }
        }};
    }
    match scalar {
        ScalarType::Int32 => varint_arm!(i32, |v| v as i64 as u64),
        ScalarType::Int64 => varint_arm!(i64, |v| v as u64),
        ScalarType::UInt32 => varint_arm!(u32, |v| v as u64),
        ScalarType::UInt64 => varint_arm!(u64, |v| v),
        ScalarType::SInt32 => varint_arm!(i32, |v| wire::zigzag_encode32(v) as u64),
        ScalarType::SInt64 => varint_arm!(i64, wire::zigzag_encode64),
        ScalarType::Bool => {
            let v: bool = value.extract()?;
            if v || !skip_default {
                wire::write_tag(buf, number, wire::WireType::Varint);
                wire::write_varint(buf, v as u64);
            }
        }
        ScalarType::Fixed32 | ScalarType::SFixed32 | ScalarType::Float => {
            let bits = match scalar {
                ScalarType::Fixed32 => value.extract::<u32>()?,
                ScalarType::SFixed32 => value.extract::<i32>()? as u32,
                _ => value.extract::<f32>()?.to_bits(),
            };
            if !(skip_default && bits == 0) {
                wire::write_tag(buf, number, wire::WireType::I32);
                wire::write_fixed32(buf, bits);
            }
        }
        ScalarType::Fixed64 | ScalarType::SFixed64 | ScalarType::Double => {
            let bits = match scalar {
                ScalarType::Fixed64 => value.extract::<u64>()?,
                ScalarType::SFixed64 => value.extract::<i64>()? as u64,
                _ => value.extract::<f64>()?.to_bits(),
            };
            if !(skip_default && bits == 0) {
                wire::write_tag(buf, number, wire::WireType::I64);
                wire::write_fixed64(buf, bits);
            }
        }
        ScalarType::String => {
            let s = value
                .downcast::<PyString>()
                .map_err(PyErr::from)?
                .to_str()?;
            if !(skip_default && s.is_empty()) {
                wire::write_tag(buf, number, wire::WireType::Len);
                wire::write_len_delimited(buf, s.as_bytes());
            }
        }
        ScalarType::Bytes => {
            let b = byte_slice(value)?;
            if !(skip_default && b.is_empty()) {
                wire::write_tag(buf, number, wire::WireType::Len);
                wire::write_len_delimited(buf, b);
            }
        }
    }
    Ok(())
}

/// Write the tag-less payload of one scalar value (packed/map elements).
fn encode_scalar(buf: &mut Vec<u8>, scalar: ScalarType, value: &Bound<'_, PyAny>) -> PyResult<()> {
    match scalar {
        ScalarType::Int32 => {
            let v: i32 = value.extract()?;
            wire::write_varint(buf, v as i64 as u64);
        }
        ScalarType::Int64 => {
            let v: i64 = value.extract()?;
            wire::write_varint(buf, v as u64);
        }
        ScalarType::UInt32 => {
            let v: u32 = value.extract()?;
            wire::write_varint(buf, v as u64);
        }
        ScalarType::UInt64 => {
            let v: u64 = value.extract()?;
            wire::write_varint(buf, v);
        }
        ScalarType::SInt32 => {
            let v: i32 = value.extract()?;
            wire::write_varint(buf, wire::zigzag_encode32(v) as u64);
        }
        ScalarType::SInt64 => {
            let v: i64 = value.extract()?;
            wire::write_varint(buf, wire::zigzag_encode64(v));
        }
        ScalarType::Bool => {
            let v: bool = value.extract()?;
            wire::write_varint(buf, v as u64);
        }
        ScalarType::Fixed32 => {
            let v: u32 = value.extract()?;
            wire::write_fixed32(buf, v);
        }
        ScalarType::SFixed32 => {
            let v: i32 = value.extract()?;
            wire::write_fixed32(buf, v as u32);
        }
        ScalarType::Float => {
            let v: f32 = value.extract()?;
            wire::write_fixed32(buf, v.to_bits());
        }
        ScalarType::Fixed64 => {
            let v: u64 = value.extract()?;
            wire::write_fixed64(buf, v);
        }
        ScalarType::SFixed64 => {
            let v: i64 = value.extract()?;
            wire::write_fixed64(buf, v as u64);
        }
        ScalarType::Double => {
            let v: f64 = value.extract()?;
            wire::write_fixed64(buf, v.to_bits());
        }
        ScalarType::String => {
            let s = value
                .downcast::<PyString>()
                .map_err(PyErr::from)?
                .to_str()?;
            wire::write_len_delimited(buf, s.as_bytes());
        }
        ScalarType::Bytes => {
            wire::write_len_delimited(buf, byte_slice(value)?);
        }
    }
    Ok(())
}
