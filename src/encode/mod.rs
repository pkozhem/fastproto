//! Encode a Python message instance into protobuf wire bytes.
//!
//! The encoder walks a [`MessageDescriptor`], reads each field off the instance
//! with `getattr`, and appends the tag + payload to the output buffer. Nested
//! messages recurse through the child's own `__fastproto__` descriptor, so no
//! pre-resolved class references are needed here.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::descriptor::{FieldKind, Label, MapValue, MessageDescriptor, ScalarType, MAX_DEPTH};
use crate::message::Descriptor;
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
    desc: &MessageDescriptor,
    buf: &mut Vec<u8>,
    depth: usize,
) -> PyResult<()> {
    if depth > MAX_DEPTH {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "message nesting exceeded {MAX_DEPTH} levels"
        )));
    }
    check_oneofs(instance, desc)?;

    for field in &desc.fields {
        let value = instance.getattr(field.name.as_str())?;

        if let FieldKind::Map { key, value: val_kind } = &field.kind {
            encode_map(py, buf, field.number, *key, val_kind, &value, depth)?;
            continue;
        }

        match field.label {
            Label::Repeated => encode_repeated(py, buf, field.number, &field.kind, &value, depth)?,
            Label::Optional => {
                if !value.is_none() {
                    encode_single(py, buf, field.number, &field.kind, &value, depth)?;
                }
            }
            Label::Single => {
                if !is_default(&field.kind, &value)? {
                    encode_single(py, buf, field.number, &field.kind, &value, depth)?;
                }
            }
        }
    }

    // Re-emit unknown fields captured by the decoder (stored on the hidden
    // `Message` slot). The slot is unset on hand-constructed instances, so an
    // AttributeError here just means "nothing to preserve".
    if let Ok(raw) = instance.getattr("_fastproto_unknown") {
        buf.extend_from_slice(&raw.extract::<Vec<u8>>()?);
    }
    Ok(())
}

/// Enforce that at most one member of each real oneof group is set.
fn check_oneofs(instance: &Bound<'_, PyAny>, desc: &MessageDescriptor) -> PyResult<()> {
    if desc.oneofs.is_empty() {
        return Ok(());
    }
    let mut set_count = vec![0u32; desc.oneofs.len()];
    for field in &desc.fields {
        if let Some(idx) = field.oneof_index {
            if !instance.getattr(field.name.as_str())?.is_none() {
                set_count[idx as usize] += 1;
            }
        }
    }
    for (idx, count) in set_count.iter().enumerate() {
        if *count > 1 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "at most one member of oneof '{}' may be set, got {}",
                desc.oneofs[idx], count
            )));
        }
    }
    Ok(())
}

/// Write a single tagged value (scalar, enum, or message).
fn encode_single(
    py: Python<'_>,
    buf: &mut Vec<u8>,
    number: u32,
    kind: &FieldKind,
    value: &Bound<'_, PyAny>,
    depth: usize,
) -> PyResult<()> {
    match kind {
        FieldKind::Scalar(scalar) => {
            wire::write_tag(buf, number, scalar.wire_type());
            encode_scalar(buf, *scalar, value)?;
        }
        FieldKind::Enum => {
            wire::write_tag(buf, number, wire::WireType::Varint);
            let v: i32 = value.extract()?;
            wire::write_varint(buf, v as i64 as u64);
        }
        FieldKind::Message => {
            let mut nested = Vec::new();
            encode_message_value(py, value, &mut nested, depth)?;
            wire::write_tag(buf, number, wire::WireType::Len);
            wire::write_len_delimited(buf, &nested);
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
    let mut nested = Vec::new();
    wellknown::encode_parts(&mut nested, secs, nanos);
    wire::write_tag(buf, number, wire::WireType::Len);
    wire::write_len_delimited(buf, &nested);
}

/// Encode a nested message by recursing through its own descriptor.
fn encode_message_value(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    out: &mut Vec<u8>,
    depth: usize,
) -> PyResult<()> {
    let handle = value.get_type().getattr("__fastproto__")?;
    let desc = handle.downcast_into::<Descriptor>().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err("nested value is not a fastproto message")
    })?;
    let desc_ref = desc.borrow();
    encode_message(py, value, &desc_ref.inner, out, depth + 1)
}

/// Encode a repeated field (packed for numeric scalars/enums, otherwise one
/// tagged entry per element).
fn encode_repeated(
    py: Python<'_>,
    buf: &mut Vec<u8>,
    number: u32,
    kind: &FieldKind,
    value: &Bound<'_, PyAny>,
    depth: usize,
) -> PyResult<()> {
    match kind {
        FieldKind::Scalar(scalar) if scalar.is_packable() => {
            let mut packed = Vec::new();
            for item in value.try_iter()? {
                encode_scalar(&mut packed, *scalar, &item?)?;
            }
            if !packed.is_empty() {
                wire::write_tag(buf, number, wire::WireType::Len);
                wire::write_len_delimited(buf, &packed);
            }
        }
        FieldKind::Scalar(scalar) => {
            // string / bytes: one length-delimited entry each.
            for item in value.try_iter()? {
                wire::write_tag(buf, number, scalar.wire_type());
                encode_scalar(buf, *scalar, &item?)?;
            }
        }
        FieldKind::Enum => {
            let mut packed = Vec::new();
            for item in value.try_iter()? {
                let v: i32 = item?.extract()?;
                wire::write_varint(&mut packed, v as i64 as u64);
            }
            if !packed.is_empty() {
                wire::write_tag(buf, number, wire::WireType::Len);
                wire::write_len_delimited(buf, &packed);
            }
        }
        FieldKind::Message => {
            for item in value.try_iter()? {
                let mut nested = Vec::new();
                encode_message_value(py, &item?, &mut nested, depth)?;
                wire::write_tag(buf, number, wire::WireType::Len);
                wire::write_len_delimited(buf, &nested);
            }
        }
        FieldKind::Timestamp => {
            for item in value.try_iter()? {
                let (secs, nanos) = wellknown::datetime_to_parts(py, &item?)?;
                write_parts_field(buf, number, secs, nanos);
            }
        }
        FieldKind::Duration => {
            for item in value.try_iter()? {
                let (secs, nanos) = wellknown::timedelta_to_parts(&item?)?;
                write_parts_field(buf, number, secs, nanos);
            }
        }
        FieldKind::Map { .. } => unreachable!("maps handled separately"),
    }
    Ok(())
}

/// Encode a `map<K, V>` field: one length-delimited entry message per pair.
fn encode_map(
    py: Python<'_>,
    buf: &mut Vec<u8>,
    number: u32,
    key: ScalarType,
    value_kind: &MapValue,
    value: &Bound<'_, PyAny>,
    depth: usize,
) -> PyResult<()> {
    let dict = value.downcast::<PyDict>().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err("map field must be a dict")
    })?;
    for (k, v) in dict.iter() {
        let mut entry = Vec::new();
        // key = field 1
        wire::write_tag(&mut entry, 1, key.wire_type());
        encode_scalar(&mut entry, key, &k)?;
        // value = field 2
        match value_kind {
            MapValue::Scalar(scalar) => {
                wire::write_tag(&mut entry, 2, scalar.wire_type());
                encode_scalar(&mut entry, *scalar, &v)?;
            }
            MapValue::Enum => {
                wire::write_tag(&mut entry, 2, wire::WireType::Varint);
                let iv: i32 = v.extract()?;
                wire::write_varint(&mut entry, iv as i64 as u64);
            }
            MapValue::Message => {
                let mut nested = Vec::new();
                encode_message_value(py, &v, &mut nested, depth)?;
                wire::write_tag(&mut entry, 2, wire::WireType::Len);
                wire::write_len_delimited(&mut entry, &nested);
            }
            MapValue::Timestamp => {
                let (secs, nanos) = wellknown::datetime_to_parts(py, &v)?;
                write_parts_field(&mut entry, 2, secs, nanos);
            }
            MapValue::Duration => {
                let (secs, nanos) = wellknown::timedelta_to_parts(&v)?;
                write_parts_field(&mut entry, 2, secs, nanos);
            }
        }
        wire::write_tag(buf, number, wire::WireType::Len);
        wire::write_len_delimited(buf, &entry);
    }
    Ok(())
}

/// Write the tag-less payload of one scalar value.
fn encode_scalar(
    buf: &mut Vec<u8>,
    scalar: ScalarType,
    value: &Bound<'_, PyAny>,
) -> PyResult<()> {
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
            let v: String = value.extract()?;
            wire::write_len_delimited(buf, v.as_bytes());
        }
        ScalarType::Bytes => {
            let v: Vec<u8> = value.extract()?;
            wire::write_len_delimited(buf, &v);
        }
    }
    Ok(())
}

/// Whether a proto3 implicit-presence value equals its type default and should
/// therefore be omitted from the output.
fn is_default(kind: &FieldKind, value: &Bound<'_, PyAny>) -> PyResult<bool> {
    match kind {
        FieldKind::Enum => Ok(value.extract::<i64>()? == 0),
        FieldKind::Scalar(scalar) => match scalar {
            ScalarType::Bool => Ok(!value.extract::<bool>()?),
            // Compare bits, not value: `-0.0 == 0.0` is true, but `-0.0` is not
            // the proto default and must be emitted (google keeps its sign).
            ScalarType::Float | ScalarType::Double => Ok(value.extract::<f64>()?.to_bits() == 0),
            ScalarType::String => Ok(value.extract::<&str>()?.is_empty()),
            ScalarType::Bytes => Ok(value.extract::<Vec<u8>>()?.is_empty()),
            _ => Ok(value.extract::<i128>()? == 0),
        },
        // Message-like fields are always Optional; maps are handled separately.
        FieldKind::Message
        | FieldKind::Timestamp
        | FieldKind::Duration
        | FieldKind::Map { .. } => Ok(false),
    }
}
