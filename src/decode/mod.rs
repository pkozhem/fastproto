//! Decode protobuf wire bytes into a Python message instance.
//!
//! The decoder parses the field stream, converts each recognised field to a
//! Python object, and collects the values per field index. Construction then
//! takes one of two routes: the fast path builds the instance with
//! `object.__new__` and fills every attribute directly (allowed only when the
//! Python side verified at link time that the class is a plain generated
//! dataclass — see `fast_init`); the general path calls the class with keyword
//! arguments so any custom `__init__`/`__post_init__` behavior is preserved.
//! Absent fields fall back to their proto3 defaults (`None` for optional
//! fields). Repeated/map fields accumulate into a list/dict that is created up
//! front and filled as matching tags arrive.

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBytes, PyDict, PyList, PyType};

use crate::descriptor::{FieldKind, Label, MapValue, ScalarType, MAX_DEPTH};
use crate::message::{Descriptor, FastInit, LinkedRef};
use crate::wellknown;
use crate::wire::{self, Reader, WireError, WireType};

static OBJECT_NEW: GILOnceCell<Py<PyAny>> = GILOnceCell::new();

/// `object.__new__`, used by the fast construction path to allocate an
/// instance without running `__init__`.
fn object_new(py: Python<'_>) -> PyResult<&Py<PyAny>> {
    OBJECT_NEW.get_or_try_init(py, || {
        py.import("builtins")?
            .getattr("object")?
            .getattr("__new__")
            .map(Bound::unbind)
    })
}

/// Decode `data` into a new instance of `cls` according to `desc`.
///
/// `depth` is the current message-nesting level (0 at the top); it is bounded
/// by [`MAX_DEPTH`] so adversarially nested input cannot exhaust the stack.
pub fn decode_message<'py>(
    py: Python<'py>,
    cls: &Bound<'py, PyType>,
    desc: &Descriptor,
    data: &[u8],
    depth: usize,
) -> PyResult<Bound<'py, PyAny>> {
    if depth > MAX_DEPTH {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "message nesting exceeded {MAX_DEPTH} levels"
        )));
    }
    let fields = &desc.inner.fields;
    let refs = desc.refs();

    // The decoded Python value per field index; None = not seen on the wire.
    // Typical messages fit on the stack, sparing a heap allocation per
    // message (including every nested one).
    let mut values_stack: [Option<Bound<'py, PyAny>>; 16] = Default::default();
    let mut values_heap;
    let values: &mut [Option<Bound<'py, PyAny>>] = if fields.len() <= 16 {
        &mut values_stack[..fields.len()]
    } else {
        values_heap = (0..fields.len()).map(|_| None).collect::<Vec<_>>();
        &mut values_heap
    };

    // Pre-create accumulators for repeated (list) and map (dict) fields.
    for (idx, field) in fields.iter().enumerate() {
        if matches!(field.kind, FieldKind::Map { .. }) {
            values[idx] = Some(PyDict::new(py).into_any());
        } else if field.label == Label::Repeated {
            values[idx] = Some(PyList::empty(py).into_any());
        }
    }

    // Raw bytes of fields the schema doesn't know about, preserved verbatim so
    // a decode -> encode round-trip keeps them (protobuf forward compatibility).
    let mut unknown = Vec::new();

    // For each oneof group, the field index currently occupying it. protobuf
    // semantics are "last one on the wire wins", so a later member of the same
    // group clears the earlier one (else the instance would have two members
    // set and fail to re-encode).
    let mut oneof_owner: HashMap<u32, usize> = HashMap::new();

    let mut reader = Reader::new(data);
    while !reader.is_empty() {
        let start = reader.pos();
        let (number, wire_type) = reader.read_tag().map_err(wire_err)?;
        let idx = match desc.field_index.get(number) {
            Some(idx) => idx,
            None => {
                reader.skip(wire_type).map_err(wire_err)?;
                unknown.extend_from_slice(reader.raw_since(start));
                continue;
            }
        };
        let field = &fields[idx];
        let linked = refs.and_then(|r| r.get(&number));

        // For singular fields, a wire type that disagrees with the schema is
        // treated as unknown (skipped and preserved) rather than mis-decoded.
        // Repeated fields validate the wire type themselves in `decode_repeated`
        // since they accept both packed and unpacked forms.
        if field.label != Label::Repeated {
            let expected = match &field.kind {
                FieldKind::Scalar(scalar) => scalar.wire_type(),
                FieldKind::Enum => WireType::Varint,
                FieldKind::Message
                | FieldKind::Timestamp
                | FieldKind::Duration
                | FieldKind::Map { .. } => WireType::Len,
            };
            if wire_type != expected {
                reader.skip(wire_type).map_err(wire_err)?;
                unknown.extend_from_slice(reader.raw_since(start));
                continue;
            }
        }

        // A oneof member about to be set clears any earlier member of its group.
        if let Some(group) = field.oneof_index {
            if let Some(prev) = oneof_owner.insert(group, idx) {
                if prev != idx {
                    values[prev] = None;
                }
            }
        }

        match &field.kind {
            FieldKind::Map { key, value } => {
                let entry = reader.read_len_delimited().map_err(wire_err)?;
                let (k, v) = decode_map_entry(py, *key, value, linked, entry, depth)?;
                let map = values[idx].as_ref().unwrap().downcast::<PyDict>().unwrap();
                map.set_item(k, v)?;
            }
            _ if field.label == Label::Repeated => {
                let list = values[idx]
                    .as_ref()
                    .unwrap()
                    .downcast::<PyList>()
                    .unwrap()
                    .clone();
                let handled = decode_repeated(
                    py,
                    &field.kind,
                    linked,
                    wire_type,
                    &mut reader,
                    &list,
                    depth,
                )?;
                if !handled {
                    // Wire type didn't match the repeated field; preserve the
                    // bytes as unknown rather than dropping them silently.
                    reader.skip(wire_type).map_err(wire_err)?;
                    unknown.extend_from_slice(reader.raw_since(start));
                }
            }
            FieldKind::Scalar(scalar) => {
                values[idx] = Some(decode_scalar(py, *scalar, &mut reader)?);
            }
            FieldKind::Enum => {
                let raw = reader.read_varint().map_err(wire_err)?;
                values[idx] = Some(coerce_enum(py, linked, raw as u32 as i32)?);
            }
            FieldKind::Message => {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                values[idx] = Some(decode_message_value(py, linked, sub, depth)?);
            }
            FieldKind::Timestamp => {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                let (secs, nanos) = wellknown::decode_parts(sub).map_err(wire_err)?;
                values[idx] = Some(wellknown::parts_to_datetime(py, secs, nanos)?);
            }
            FieldKind::Duration => {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                let (secs, nanos) = wellknown::decode_parts(sub).map_err(wire_err)?;
                values[idx] = Some(wellknown::parts_to_timedelta(py, secs, nanos)?);
            }
        }
    }

    if let Some(fast_init) = desc.fast_init.get() {
        // The fast path was verified for exactly one class; a subclass
        // sharing this descriptor goes through its own __init__ below.
        if cls.is(fast_init.cls.bind(py)) {
            return construct_fast(py, desc, fast_init, values, &unknown);
        }
    }

    let kwargs = PyDict::new(py);
    for (idx, value) in values.iter_mut().enumerate() {
        if let Some(value) = value.take() {
            kwargs.set_item(desc.field_names[idx].bind(py), value)?;
        }
    }
    let instance = cls.call((), Some(&kwargs))?;
    if !unknown.is_empty() {
        instance.setattr(
            pyo3::intern!(py, "_fastproto_unknown"),
            PyBytes::new(py, &unknown),
        )?;
    }
    Ok(instance)
}

/// Build the instance with `object.__new__` + per-field setattr, skipping the
/// dataclass `__init__` entirely. Only reached when the Python side verified
/// at link time that this is observably equivalent for the class (plain
/// generated dataclass, default `__new__`, no custom `__post_init__`) and
/// captured the class's own default objects — so an absent field gets the
/// exact same object `__init__` would have assigned.
fn construct_fast<'py>(
    py: Python<'py>,
    desc: &Descriptor,
    fast_init: &FastInit,
    values: &mut [Option<Bound<'py, PyAny>>],
    unknown: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let instance = object_new(py)?
        .bind(py)
        .call1(fast_init.new_args.bind(py))?;
    for (idx, value) in values.iter_mut().enumerate() {
        let name = desc.field_names[idx].bind(py);
        match value.take() {
            Some(value) => instance.setattr(name, value)?,
            None => instance.setattr(name, fast_init.defaults[idx].bind(py))?,
        }
    }
    // Always fill the unknown-fields slot: encode reads it on every call, and
    // a set slot keeps that read exception-free.
    instance.setattr(
        pyo3::intern!(py, "_fastproto_unknown"),
        PyBytes::new(py, unknown),
    )?;
    Ok(instance)
}

/// Append one or more repeated elements (handling packed encoding).
///
/// Returns `false` (without consuming any bytes) when `wire_type` doesn't match
/// this field, so the caller can preserve the raw bytes as an unknown field
/// instead of dropping them.
fn decode_repeated<'py>(
    py: Python<'py>,
    kind: &FieldKind,
    linked: Option<&LinkedRef>,
    wire_type: WireType,
    reader: &mut Reader<'_>,
    list: &Bound<'py, PyList>,
    depth: usize,
) -> PyResult<bool> {
    match kind {
        FieldKind::Scalar(scalar) if scalar.is_packable() => {
            if wire_type == WireType::Len {
                let chunk = reader.read_len_delimited().map_err(wire_err)?;
                let mut r = Reader::new(chunk);
                while !r.is_empty() {
                    list.append(decode_scalar(py, *scalar, &mut r)?)?;
                }
            } else if wire_type == scalar.wire_type() {
                list.append(decode_scalar(py, *scalar, reader)?)?;
            } else {
                return Ok(false);
            }
        }
        FieldKind::Scalar(scalar) => {
            // string / bytes
            if wire_type == WireType::Len {
                list.append(decode_scalar(py, *scalar, reader)?)?;
            } else {
                return Ok(false);
            }
        }
        FieldKind::Enum => {
            if wire_type == WireType::Len {
                let chunk = reader.read_len_delimited().map_err(wire_err)?;
                let mut r = Reader::new(chunk);
                while !r.is_empty() {
                    let raw = r.read_varint().map_err(wire_err)?;
                    list.append(coerce_enum(py, linked, raw as u32 as i32)?)?;
                }
            } else if wire_type == WireType::Varint {
                let raw = reader.read_varint().map_err(wire_err)?;
                list.append(coerce_enum(py, linked, raw as u32 as i32)?)?;
            } else {
                return Ok(false);
            }
        }
        FieldKind::Message => {
            if wire_type == WireType::Len {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                list.append(decode_message_value(py, linked, sub, depth)?)?;
            } else {
                return Ok(false);
            }
        }
        FieldKind::Timestamp | FieldKind::Duration => {
            if wire_type == WireType::Len {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                let (secs, nanos) = wellknown::decode_parts(sub).map_err(wire_err)?;
                list.append(match kind {
                    FieldKind::Timestamp => wellknown::parts_to_datetime(py, secs, nanos)?,
                    _ => wellknown::parts_to_timedelta(py, secs, nanos)?,
                })?;
            } else {
                return Ok(false);
            }
        }
        FieldKind::Map { .. } => unreachable!("maps handled separately"),
    }
    Ok(true)
}

/// Decode a single `map<K, V>` entry message into `(key, value)` objects.
fn decode_map_entry<'py>(
    py: Python<'py>,
    key: ScalarType,
    value_kind: &MapValue,
    linked: Option<&LinkedRef>,
    entry: &[u8],
    depth: usize,
) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let mut reader = Reader::new(entry);
    let mut key_obj: Option<Bound<'py, PyAny>> = None;
    let mut val_obj: Option<Bound<'py, PyAny>> = None;

    while !reader.is_empty() {
        let (number, wire) = reader.read_tag().map_err(wire_err)?;
        match number {
            1 if wire == key.wire_type() => key_obj = Some(decode_scalar(py, key, &mut reader)?),
            2 => match value_kind {
                MapValue::Scalar(scalar) if wire == scalar.wire_type() => {
                    val_obj = Some(decode_scalar(py, *scalar, &mut reader)?);
                }
                MapValue::Enum if wire == WireType::Varint => {
                    let raw = reader.read_varint().map_err(wire_err)?;
                    val_obj = Some(coerce_enum(py, linked, raw as u32 as i32)?);
                }
                MapValue::Message if wire == WireType::Len => {
                    let sub = reader.read_len_delimited().map_err(wire_err)?;
                    val_obj = Some(decode_message_value(py, linked, sub, depth)?);
                }
                MapValue::Timestamp | MapValue::Duration if wire == WireType::Len => {
                    let sub = reader.read_len_delimited().map_err(wire_err)?;
                    let (secs, nanos) = wellknown::decode_parts(sub).map_err(wire_err)?;
                    val_obj = Some(match value_kind {
                        MapValue::Timestamp => wellknown::parts_to_datetime(py, secs, nanos)?,
                        _ => wellknown::parts_to_timedelta(py, secs, nanos)?,
                    });
                }
                _ => reader.skip(wire).map_err(wire_err)?,
            },
            _ => reader.skip(wire).map_err(wire_err)?,
        }
    }

    let key_obj = match key_obj {
        Some(k) => k,
        None => scalar_default(py, key)?,
    };
    let val_obj = match val_obj {
        Some(v) => v,
        None => match value_kind {
            MapValue::Scalar(scalar) => scalar_default(py, *scalar)?,
            MapValue::Enum => coerce_enum(py, linked, 0)?,
            MapValue::Message => decode_message_value(py, linked, &[], depth)?,
            MapValue::Timestamp => wellknown::parts_to_datetime(py, 0, 0)?,
            MapValue::Duration => wellknown::parts_to_timedelta(py, 0, 0)?,
        },
    };
    Ok((key_obj, val_obj))
}

/// Coerce an integer to its Python `IntEnum` subclass.
///
/// proto3 enums are open: an undefined numeric value is valid on the wire, so
/// an unmapped value stays a raw int (mirroring google's Python runtime). The
/// raw int also survives re-encoding, since the encoder extracts a plain `i32`
/// from either form. Enums with the default `_missing_` resolve through the
/// cached `_value2member_map_` dict — one lookup, no exception; others go
/// through `EnumMeta.__call__` so a custom hook stays observable.
fn coerce_enum<'py>(
    py: Python<'py>,
    linked: Option<&LinkedRef>,
    value: i32,
) -> PyResult<Bound<'py, PyAny>> {
    if let Some(linked) = linked {
        // Direct member table for small non-negative values (a negative
        // `value as usize` wraps far past the table and falls through).
        if let Some(table) = &linked.enum_table {
            if let Some(slot) = table.get(value as usize) {
                return match slot {
                    Some(member) => Ok(member.bind(py).clone()),
                    None => Ok(value.into_pyobject(py)?.into_any()),
                };
            }
        }
        if let Some(map) = &linked.enum_map {
            if let Some(member) = map.bind(py).get_item(value)? {
                return Ok(member);
            }
        } else {
            match linked.class.bind(py).call1((value,)) {
                Ok(member) => return Ok(member),
                Err(err) if err.is_instance_of::<pyo3::exceptions::PyValueError>(py) => {}
                Err(err) => return Err(err),
            }
        }
    }
    Ok(value.into_pyobject(py)?.into_any())
}

/// Decode a nested message through its class's cached descriptor.
fn decode_message_value<'py>(
    py: Python<'py>,
    linked: Option<&LinkedRef>,
    data: &[u8],
    depth: usize,
) -> PyResult<Bound<'py, PyAny>> {
    let linked = linked.ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("message field was not linked to a class")
    })?;
    let cls = linked.class.bind(py);
    let ty = cls.downcast::<PyType>()?;
    match &linked.desc {
        Some(desc) => {
            let desc_ref = desc.bind(py).borrow();
            decode_message(py, ty, &desc_ref, data, depth + 1)
        }
        None => Err(pyo3::exceptions::PyTypeError::new_err(
            "linked class is not a fastproto message",
        )),
    }
}

/// The Python default object for a scalar (used for omitted map keys/values
/// and absent fields on the fast construction path).
fn scalar_default<'py>(py: Python<'py>, scalar: ScalarType) -> PyResult<Bound<'py, PyAny>> {
    Ok(match scalar {
        ScalarType::Float | ScalarType::Double => 0.0_f64.into_pyobject(py)?.into_any(),
        ScalarType::Bool => false.into_pyobject(py)?.to_owned().into_any(),
        ScalarType::String => pyo3::intern!(py, "").clone().into_any(),
        ScalarType::Bytes => PyBytes::new(py, &[]).into_any(),
        _ => 0_i64.into_pyobject(py)?.into_any(),
    })
}

/// Read one scalar payload off the reader and convert it to a Python object.
fn decode_scalar<'py>(
    py: Python<'py>,
    scalar: ScalarType,
    reader: &mut Reader<'_>,
) -> PyResult<Bound<'py, PyAny>> {
    let obj = match scalar {
        ScalarType::Int32 => {
            let raw = reader.read_varint().map_err(wire_err)?;
            ((raw as u32) as i32).into_pyobject(py)?.into_any()
        }
        ScalarType::Int64 => {
            let raw = reader.read_varint().map_err(wire_err)?;
            (raw as i64).into_pyobject(py)?.into_any()
        }
        ScalarType::UInt32 => {
            let raw = reader.read_varint().map_err(wire_err)?;
            (raw as u32).into_pyobject(py)?.into_any()
        }
        ScalarType::UInt64 => {
            let raw = reader.read_varint().map_err(wire_err)?;
            raw.into_pyobject(py)?.into_any()
        }
        ScalarType::SInt32 => {
            let raw = reader.read_varint().map_err(wire_err)?;
            wire::zigzag_decode32(raw as u32)
                .into_pyobject(py)?
                .into_any()
        }
        ScalarType::SInt64 => {
            let raw = reader.read_varint().map_err(wire_err)?;
            wire::zigzag_decode64(raw).into_pyobject(py)?.into_any()
        }
        ScalarType::Bool => {
            let raw = reader.read_varint().map_err(wire_err)?;
            (raw != 0).into_pyobject(py)?.to_owned().into_any()
        }
        ScalarType::Fixed32 => {
            let raw = reader.read_fixed32().map_err(wire_err)?;
            raw.into_pyobject(py)?.into_any()
        }
        ScalarType::SFixed32 => {
            let raw = reader.read_fixed32().map_err(wire_err)?;
            (raw as i32).into_pyobject(py)?.into_any()
        }
        ScalarType::Float => {
            let raw = reader.read_fixed32().map_err(wire_err)?;
            f32::from_bits(raw).into_pyobject(py)?.into_any()
        }
        ScalarType::Fixed64 => {
            let raw = reader.read_fixed64().map_err(wire_err)?;
            raw.into_pyobject(py)?.into_any()
        }
        ScalarType::SFixed64 => {
            let raw = reader.read_fixed64().map_err(wire_err)?;
            (raw as i64).into_pyobject(py)?.into_any()
        }
        ScalarType::Double => {
            let raw = reader.read_fixed64().map_err(wire_err)?;
            f64::from_bits(raw).into_pyobject(py)?.into_any()
        }
        ScalarType::String => {
            let bytes = reader.read_len_delimited().map_err(wire_err)?;
            let text = std::str::from_utf8(bytes).map_err(|_| {
                pyo3::exceptions::PyValueError::new_err("invalid utf-8 in string field")
            })?;
            text.into_pyobject(py)?.into_any()
        }
        ScalarType::Bytes => {
            let bytes = reader.read_len_delimited().map_err(wire_err)?;
            PyBytes::new(py, bytes).into_any()
        }
    };
    Ok(obj)
}

/// Map a low-level wire error to a Python exception.
fn wire_err(err: WireError) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(format!("malformed protobuf data: {err:?}"))
}
