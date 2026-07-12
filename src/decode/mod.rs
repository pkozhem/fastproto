//! Decode protobuf wire bytes into a Python message instance.
//!
//! The decoder parses the field stream, converts each recognised field to a
//! Python object, and constructs the dataclass with the collected values as
//! keyword arguments. Absent fields are left out of the kwargs, so the
//! dataclass falls back to its declared defaults (proto3 semantics, and `None`
//! for optional fields). Repeated/map fields accumulate into a list/dict that
//! is created up front and filled as matching tags arrive.

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyType};

use crate::descriptor::{FieldKind, Label, MapValue, MessageDescriptor, ScalarType, MAX_DEPTH};
use crate::message::Descriptor;
use crate::wellknown;
use crate::wire::{self, Reader, WireError, WireType};

type Refs = HashMap<u32, Py<PyAny>>;

/// Decode `data` into a new instance of `cls` according to `desc`.
///
/// `depth` is the current message-nesting level (0 at the top); it is bounded
/// by [`MAX_DEPTH`] so adversarially nested input cannot exhaust the stack.
pub fn decode_message<'py>(
    py: Python<'py>,
    cls: &Bound<'py, PyType>,
    desc: &MessageDescriptor,
    refs: &Refs,
    data: &[u8],
    depth: usize,
) -> PyResult<Bound<'py, PyAny>> {
    if depth > MAX_DEPTH {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "message nesting exceeded {MAX_DEPTH} levels"
        )));
    }
    let kwargs = PyDict::new(py);

    // Pre-create accumulators for repeated (list) and map (dict) fields.
    let mut lists: HashMap<u32, Bound<'py, PyList>> = HashMap::new();
    let mut maps: HashMap<u32, Bound<'py, PyDict>> = HashMap::new();
    for field in &desc.fields {
        if matches!(field.kind, FieldKind::Map { .. }) {
            let d = PyDict::new(py);
            kwargs.set_item(field.name.as_str(), &d)?;
            maps.insert(field.number, d);
        } else if field.label == Label::Repeated {
            let l = PyList::empty(py);
            kwargs.set_item(field.name.as_str(), &l)?;
            lists.insert(field.number, l);
        }
    }

    // Raw bytes of fields the schema doesn't know about, preserved verbatim so
    // a decode -> encode round-trip keeps them (protobuf forward compatibility).
    let mut unknown = Vec::new();

    // For each oneof group, the field name currently occupying it. protobuf
    // semantics are "last one on the wire wins", so a later member of the same
    // group clears the earlier one from the kwargs (else the instance would
    // have two members set and fail to re-encode).
    let mut oneof_owner: HashMap<u32, &str> = HashMap::new();

    let mut reader = Reader::new(data);
    while !reader.is_empty() {
        let start = reader.pos();
        let (number, wire_type) = reader.read_tag().map_err(wire_err)?;
        let field = match desc.field_by_number(number) {
            Some(f) => f,
            None => {
                reader.skip(wire_type).map_err(wire_err)?;
                unknown.extend_from_slice(reader.raw_since(start));
                continue;
            }
        };

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
            if let Some(prev) = oneof_owner.insert(group, field.name.as_str()) {
                if prev != field.name.as_str() {
                    kwargs.del_item(prev)?;
                }
            }
        }

        match &field.kind {
            FieldKind::Map { key, value } => {
                let entry = reader.read_len_delimited().map_err(wire_err)?;
                let (k, v) = decode_map_entry(py, *key, value, refs.get(&number), entry, depth)?;
                maps.get(&number).unwrap().set_item(k, v)?;
            }
            _ if field.label == Label::Repeated => {
                let list = lists.get(&number).unwrap();
                let handled = decode_repeated(
                    py,
                    &field.kind,
                    refs.get(&number),
                    wire_type,
                    &mut reader,
                    list,
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
                let value = decode_scalar(py, *scalar, &mut reader)?;
                kwargs.set_item(field.name.as_str(), value)?;
            }
            FieldKind::Enum => {
                let raw = reader.read_varint().map_err(wire_err)?;
                let value = coerce_enum(py, refs.get(&number), raw as u32 as i32)?;
                kwargs.set_item(field.name.as_str(), value)?;
            }
            FieldKind::Message => {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                let value = decode_message_value(py, refs.get(&number), sub, depth)?;
                kwargs.set_item(field.name.as_str(), value)?;
            }
            FieldKind::Timestamp => {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                let (secs, nanos) = wellknown::decode_parts(sub).map_err(wire_err)?;
                kwargs.set_item(
                    field.name.as_str(),
                    wellknown::parts_to_datetime(py, secs, nanos)?,
                )?;
            }
            FieldKind::Duration => {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                let (secs, nanos) = wellknown::decode_parts(sub).map_err(wire_err)?;
                kwargs.set_item(
                    field.name.as_str(),
                    wellknown::parts_to_timedelta(py, secs, nanos)?,
                )?;
            }
        }
    }

    let instance = cls.call((), Some(&kwargs))?;
    if !unknown.is_empty() {
        instance.setattr("_fastproto_unknown", PyBytes::new(py, &unknown))?;
    }
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
    class_ref: Option<&Py<PyAny>>,
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
                    list.append(coerce_enum(py, class_ref, raw as u32 as i32)?)?;
                }
            } else if wire_type == WireType::Varint {
                let raw = reader.read_varint().map_err(wire_err)?;
                list.append(coerce_enum(py, class_ref, raw as u32 as i32)?)?;
            } else {
                return Ok(false);
            }
        }
        FieldKind::Message => {
            if wire_type == WireType::Len {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                list.append(decode_message_value(py, class_ref, sub, depth)?)?;
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
    class_ref: Option<&Py<PyAny>>,
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
                    val_obj = Some(coerce_enum(py, class_ref, raw as u32 as i32)?);
                }
                MapValue::Message if wire == WireType::Len => {
                    let sub = reader.read_len_delimited().map_err(wire_err)?;
                    val_obj = Some(decode_message_value(py, class_ref, sub, depth)?);
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
            MapValue::Enum => coerce_enum(py, class_ref, 0)?,
            MapValue::Message => decode_message_value(py, class_ref, &[], depth)?,
            MapValue::Timestamp => wellknown::parts_to_datetime(py, 0, 0)?,
            MapValue::Duration => wellknown::parts_to_timedelta(py, 0, 0)?,
        },
    };
    Ok((key_obj, val_obj))
}

/// Coerce an integer to its Python `IntEnum` subclass.
///
/// proto3 enums are open: an undefined numeric value is valid on the wire, so
/// when `IntEnum(value)` raises `ValueError` we keep the raw int (mirroring
/// google's Python runtime). The raw int also survives re-encoding, since the
/// encoder extracts a plain `i32` from either form.
fn coerce_enum<'py>(
    py: Python<'py>,
    class_ref: Option<&Py<PyAny>>,
    value: i32,
) -> PyResult<Bound<'py, PyAny>> {
    if let Some(cls) = class_ref {
        match cls.bind(py).call1((value,)) {
            Ok(member) => return Ok(member),
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyValueError>(py) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(value.into_pyobject(py)?.into_any())
}

/// Decode a nested message by recursing through its class's descriptor.
fn decode_message_value<'py>(
    py: Python<'py>,
    class_ref: Option<&Py<PyAny>>,
    data: &[u8],
    depth: usize,
) -> PyResult<Bound<'py, PyAny>> {
    let cls = class_ref.ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("message field was not linked to a class")
    })?;
    let cls = cls.bind(py);
    let handle = cls.getattr("__fastproto__")?;
    let desc = handle.downcast_into::<Descriptor>().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err("linked class is not a fastproto message")
    })?;
    let desc_ref = desc.borrow();
    let ty = cls.downcast::<PyType>()?;
    let empty = HashMap::new();
    let refs = desc_ref.refs.get().unwrap_or(&empty);
    decode_message(py, ty, &desc_ref.inner, refs, data, depth + 1)
}

/// The Python default object for a scalar (used for omitted map keys/values).
fn scalar_default<'py>(py: Python<'py>, scalar: ScalarType) -> PyResult<Bound<'py, PyAny>> {
    Ok(match scalar {
        ScalarType::Float | ScalarType::Double => 0.0_f64.into_pyobject(py)?.into_any(),
        ScalarType::Bool => false.into_pyobject(py)?.to_owned().into_any(),
        ScalarType::String => "".into_pyobject(py)?.into_any(),
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
