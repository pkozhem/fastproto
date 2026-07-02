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

use crate::descriptor::{FieldKind, Label, MapValue, MessageDescriptor, ScalarType};
use crate::message::Descriptor;
use crate::wire::{self, Reader, WireError, WireType};

type Refs = HashMap<u32, Py<PyAny>>;

/// Decode `data` into a new instance of `cls` according to `desc`.
pub fn decode_message<'py>(
    py: Python<'py>,
    cls: &Bound<'py, PyType>,
    desc: &MessageDescriptor,
    refs: &Refs,
    data: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
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

    // Singular message fields are accumulated raw here so that repeated
    // occurrences of the same field merge (proto semantics: concatenated
    // message encodings behave like a recursive merge) instead of overwriting.
    let mut msg_bufs: HashMap<u32, Vec<u8>> = HashMap::new();

    let mut reader = Reader::new(data);
    while !reader.is_empty() {
        let (number, wire_type) = reader.read_tag().map_err(wire_err)?;
        let field = match desc.field_by_number(number) {
            Some(f) => f,
            None => {
                reader.skip(wire_type).map_err(wire_err)?;
                continue;
            }
        };

        match &field.kind {
            FieldKind::Map { key, value } => {
                let entry = reader.read_len_delimited().map_err(wire_err)?;
                let (k, v) = decode_map_entry(py, *key, value, refs.get(&number), entry)?;
                maps.get(&number).unwrap().set_item(k, v)?;
            }
            _ if field.label == Label::Repeated => {
                let list = lists.get(&number).unwrap();
                decode_repeated(
                    py,
                    &field.kind,
                    refs.get(&number),
                    wire_type,
                    &mut reader,
                    list,
                )?;
            }
            FieldKind::Scalar(scalar) => {
                if wire_type != scalar.wire_type() {
                    reader.skip(wire_type).map_err(wire_err)?;
                    continue;
                }
                let value = decode_scalar(py, *scalar, &mut reader)?;
                kwargs.set_item(field.name.as_str(), value)?;
            }
            FieldKind::Enum => {
                if wire_type != WireType::Varint {
                    reader.skip(wire_type).map_err(wire_err)?;
                    continue;
                }
                let raw = reader.read_varint().map_err(wire_err)?;
                let value = coerce_enum(py, refs.get(&number), raw as u32 as i32)?;
                kwargs.set_item(field.name.as_str(), value)?;
            }
            FieldKind::Message => {
                if wire_type != WireType::Len {
                    reader.skip(wire_type).map_err(wire_err)?;
                    continue;
                }
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                msg_bufs.entry(number).or_default().extend_from_slice(sub);
            }
        }
    }

    // Decode each singular message field once, from the merged bytes.
    for (number, bytes) in &msg_bufs {
        let field = desc
            .field_by_number(*number)
            .expect("buffered a field we looked up during the scan");
        let value = decode_message_value(py, refs.get(number), bytes)?;
        kwargs.set_item(field.name.as_str(), value)?;
    }

    cls.call((), Some(&kwargs))
}

/// Append one or more repeated elements (handling packed encoding).
fn decode_repeated<'py>(
    py: Python<'py>,
    kind: &FieldKind,
    class_ref: Option<&Py<PyAny>>,
    wire_type: WireType,
    reader: &mut Reader<'_>,
    list: &Bound<'py, PyList>,
) -> PyResult<()> {
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
                reader.skip(wire_type).map_err(wire_err)?;
            }
        }
        FieldKind::Scalar(scalar) => {
            // string / bytes
            if wire_type == WireType::Len {
                list.append(decode_scalar(py, *scalar, reader)?)?;
            } else {
                reader.skip(wire_type).map_err(wire_err)?;
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
                reader.skip(wire_type).map_err(wire_err)?;
            }
        }
        FieldKind::Message => {
            if wire_type == WireType::Len {
                let sub = reader.read_len_delimited().map_err(wire_err)?;
                list.append(decode_message_value(py, class_ref, sub)?)?;
            } else {
                reader.skip(wire_type).map_err(wire_err)?;
            }
        }
        FieldKind::Map { .. } => unreachable!("maps handled separately"),
    }
    Ok(())
}

/// Decode a single `map<K, V>` entry message into `(key, value)` objects.
fn decode_map_entry<'py>(
    py: Python<'py>,
    key: ScalarType,
    value_kind: &MapValue,
    class_ref: Option<&Py<PyAny>>,
    entry: &[u8],
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
                    val_obj = Some(decode_message_value(py, class_ref, sub)?);
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
            MapValue::Message => decode_message_value(py, class_ref, &[])?,
        },
    };
    Ok((key_obj, val_obj))
}

/// Coerce an integer to its Python `IntEnum` subclass.
///
/// proto3 enums are *open*: a value with no named member must be preserved
/// rather than rejected, so an unknown value (which makes `IntEnum(value)`
/// raise `ValueError`) falls back to the raw int. The same fallback covers an
/// enum class that was somehow left unresolved.
fn coerce_enum<'py>(
    py: Python<'py>,
    class_ref: Option<&Py<PyAny>>,
    value: i32,
) -> PyResult<Bound<'py, PyAny>> {
    match class_ref {
        Some(cls) => match cls.bind(py).call1((value,)) {
            Ok(member) => Ok(member),
            Err(err) if err.is_instance_of::<pyo3::exceptions::PyValueError>(py) => {
                Ok(value.into_pyobject(py)?.into_any())
            }
            Err(err) => Err(err),
        },
        None => Ok(value.into_pyobject(py)?.into_any()),
    }
}

/// Decode a nested message by recursing through its class's descriptor.
fn decode_message_value<'py>(
    py: Python<'py>,
    class_ref: Option<&Py<PyAny>>,
    data: &[u8],
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
    decode_message(py, ty, &desc_ref.inner, &desc_ref.refs, data)
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
                pyo3::exceptions::PyUnicodeDecodeError::new_err("invalid utf-8 in string field")
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
