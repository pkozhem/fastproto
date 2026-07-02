//! Hand-written parser for the subset of `descriptor.proto` we need.
//!
//! The protoc plugin embeds, for each message, the standard `DescriptorProto`
//! bytes that protoc already produced. Rather than run those through a generic
//! engine (which would create a bootstrap cycle), we parse the handful of
//! fields we care about directly with the wire [`Reader`]. This is the "B1"
//! path: standard descriptor bytes in, our [`MessageDescriptor`] out.
//!
//! Field numbers below come straight from `descriptor.proto`:
//! * `DescriptorProto`: name=1, field=2, nested_type=3, oneof_decl=8, options=7
//! * `FieldDescriptorProto`: name=1, number=3, label=4, type=5, type_name=6,
//!   oneof_index=9, proto3_optional=17
//! * `MessageOptions`: map_entry=7
//! * `OneofDescriptorProto`: name=1

use std::collections::HashMap;

use crate::descriptor::{
    FieldDescriptor, FieldKind, Label, MapValue, MessageDescriptor, ScalarType,
};
use crate::wire::{Reader, WireError, WireType};

/// Error while parsing a descriptor.
#[derive(Debug, PartialEq)]
pub enum ParseError {
    Wire(WireError),
    /// A field type we don't understand (e.g. group).
    UnsupportedType(i32),
    /// The `type` field of a `FieldDescriptorProto` was missing.
    MissingType,
    /// A string field was not valid UTF-8.
    InvalidUtf8,
}

impl From<WireError> for ParseError {
    fn from(err: WireError) -> ParseError {
        ParseError::Wire(err)
    }
}

// FieldDescriptorProto.Label
const LABEL_REPEATED: i64 = 3;
// FieldDescriptorProto.Type
const TYPE_GROUP: i64 = 10;
const TYPE_MESSAGE: i64 = 11;
const TYPE_ENUM: i64 = 14;

fn scalar_from_proto_type(type_code: i64) -> Option<ScalarType> {
    Some(match type_code {
        1 => ScalarType::Double,
        2 => ScalarType::Float,
        3 => ScalarType::Int64,
        4 => ScalarType::UInt64,
        5 => ScalarType::Int32,
        6 => ScalarType::Fixed64,
        7 => ScalarType::Fixed32,
        8 => ScalarType::Bool,
        9 => ScalarType::String,
        12 => ScalarType::Bytes,
        13 => ScalarType::UInt32,
        15 => ScalarType::SFixed32,
        16 => ScalarType::SFixed64,
        17 => ScalarType::SInt32,
        18 => ScalarType::SInt64,
        _ => return None,
    })
}

fn read_string(reader: &mut Reader<'_>) -> Result<String, ParseError> {
    let bytes = reader.read_len_delimited()?;
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|_| ParseError::InvalidUtf8)
}

/// `.pkg.Outer.Inner` -> `Inner`.
fn short_name(full: &str) -> String {
    full.rsplit('.').next().unwrap_or(full).to_string()
}

/// The component enclosing the leaf of a fully-qualified name:
/// `.pkg.Outer.Inner` -> `Outer`. Returns `None` if there is no such component.
fn parent_component(full: &str) -> Option<&str> {
    let mut parts = full.rsplit('.');
    parts.next()?; // drop the leaf
    parts.next()
}

/// Raw fields collected from a `FieldDescriptorProto` before interpretation.
#[derive(Default)]
struct RawField {
    name: String,
    number: u32,
    label: i64,
    type_code: Option<i64>,
    type_name: Option<String>,
    oneof_index: Option<u32>,
    proto3_optional: bool,
}

fn parse_raw_field(bytes: &[u8]) -> Result<RawField, ParseError> {
    let mut reader = Reader::new(bytes);
    let mut raw = RawField::default();
    while !reader.is_empty() {
        let (number, wire) = reader.read_tag()?;
        match (number, wire) {
            (1, WireType::Len) => raw.name = read_string(&mut reader)?,
            (3, WireType::Varint) => raw.number = reader.read_varint()? as u32,
            (4, WireType::Varint) => raw.label = reader.read_varint()? as i64,
            (5, WireType::Varint) => raw.type_code = Some(reader.read_varint()? as i64),
            (6, WireType::Len) => raw.type_name = Some(read_string(&mut reader)?),
            (9, WireType::Varint) => raw.oneof_index = Some(reader.read_varint()? as u32),
            (17, WireType::Varint) => raw.proto3_optional = reader.read_varint()? != 0,
            (_, w) => reader.skip(w)?,
        }
    }
    Ok(raw)
}

/// A nested `*Entry` type generated for a `map<K, V>` field.
struct MapEntry {
    key: ScalarType,
    value: MapValue,
    /// Short type name of the value when it is a message/enum.
    value_type_name: Option<String>,
}

/// Parse a nested `DescriptorProto`, returning a map entry if it is one.
fn parse_nested_map_entry(bytes: &[u8]) -> Result<Option<(String, MapEntry)>, ParseError> {
    let mut reader = Reader::new(bytes);
    let mut name = String::new();
    let mut is_map_entry = false;
    let mut key: Option<ScalarType> = None;
    let mut value: Option<MapValue> = None;
    let mut value_type_name: Option<String> = None;

    while !reader.is_empty() {
        let (number, wire) = reader.read_tag()?;
        match (number, wire) {
            (1, WireType::Len) => name = read_string(&mut reader)?,
            (2, WireType::Len) => {
                // A field of the entry: key (number 1) or value (number 2).
                let raw = parse_raw_field(reader.read_len_delimited()?)?;
                let type_code = raw.type_code.ok_or(ParseError::MissingType)?;
                if raw.number == 1 {
                    key = scalar_from_proto_type(type_code);
                } else if raw.number == 2 {
                    value = Some(match type_code {
                        TYPE_ENUM => MapValue::Enum,
                        TYPE_MESSAGE => MapValue::Message,
                        other => MapValue::Scalar(
                            scalar_from_proto_type(other)
                                .ok_or(ParseError::UnsupportedType(other as i32))?,
                        ),
                    });
                    value_type_name = raw.type_name.as_deref().map(short_name);
                }
            }
            (7, WireType::Len) => {
                // MessageOptions: look for map_entry (field 7, bool).
                let opts = reader.read_len_delimited()?;
                let mut o = Reader::new(opts);
                while !o.is_empty() {
                    let (n, w) = o.read_tag()?;
                    if n == 7 && w == WireType::Varint {
                        is_map_entry = o.read_varint()? != 0;
                    } else {
                        o.skip(w)?;
                    }
                }
            }
            (_, w) => reader.skip(w)?,
        }
    }

    if is_map_entry {
        let key = key.unwrap_or(ScalarType::String);
        let value = value.unwrap_or(MapValue::Scalar(ScalarType::String));
        Ok(Some((
            name,
            MapEntry {
                key,
                value,
                value_type_name,
            },
        )))
    } else {
        Ok(None)
    }
}

/// Turn a raw field plus the message's map-entry table into a [`FieldDescriptor`].
fn interpret_field(
    raw: RawField,
    message_name: &str,
    map_entries: &HashMap<String, MapEntry>,
) -> Result<FieldDescriptor, ParseError> {
    let type_code = raw.type_code.ok_or(ParseError::MissingType)?;
    let is_repeated = raw.label == LABEL_REPEATED;

    // Detect maps: a repeated message whose type is *this message's own* nested
    // map_entry. The synthetic entry's type name is `....<message>.<Entry>`, so
    // its parent component is the enclosing message; requiring that guards
    // against a real, unrelated type whose short name happens to collide with a
    // map-entry name (e.g. a top-level `FooEntry` alongside a `map` field).
    if is_repeated && type_code == TYPE_MESSAGE {
        if let Some(full) = &raw.type_name {
            let is_own_entry = parent_component(full) == Some(message_name);
            if let Some(entry) = map_entries.get(&short_name(full)).filter(|_| is_own_entry) {
                return Ok(FieldDescriptor {
                    number: raw.number,
                    name: raw.name,
                    kind: FieldKind::Map {
                        key: entry.key,
                        value: entry.value.clone(),
                    },
                    label: Label::Single,
                    type_name: entry.value_type_name.clone(),
                    oneof_index: None,
                });
            }
        }
    }

    let (kind, type_name) = match type_code {
        TYPE_ENUM => (FieldKind::Enum, raw.type_name.as_deref().map(short_name)),
        TYPE_MESSAGE => (FieldKind::Message, raw.type_name.as_deref().map(short_name)),
        TYPE_GROUP => return Err(ParseError::UnsupportedType(TYPE_GROUP as i32)),
        other => {
            let scalar =
                scalar_from_proto_type(other).ok_or(ParseError::UnsupportedType(other as i32))?;
            (FieldKind::Scalar(scalar), None)
        }
    };

    // A real oneof member has an index and is not a proto3 `optional`
    // (which protoc models with a synthetic single-member oneof).
    let real_oneof = raw.oneof_index.filter(|_| !raw.proto3_optional);

    let label = if is_repeated {
        Label::Repeated
    } else if raw.proto3_optional || matches!(kind, FieldKind::Message) || real_oneof.is_some() {
        Label::Optional
    } else {
        Label::Single
    };

    Ok(FieldDescriptor {
        number: raw.number,
        name: raw.name,
        kind,
        label,
        type_name,
        oneof_index: real_oneof,
    })
}

/// Parse a `DescriptorProto` into a [`MessageDescriptor`].
pub fn parse_message(bytes: &[u8]) -> Result<MessageDescriptor, ParseError> {
    let mut reader = Reader::new(bytes);
    let mut name = String::new();
    let mut raw_fields = Vec::new();
    let mut map_entries: HashMap<String, MapEntry> = HashMap::new();
    let mut oneofs = Vec::new();

    while !reader.is_empty() {
        let (number, wire) = reader.read_tag()?;
        match (number, wire) {
            (1, WireType::Len) => name = read_string(&mut reader)?,
            (2, WireType::Len) => {
                raw_fields.push(parse_raw_field(reader.read_len_delimited()?)?);
            }
            (3, WireType::Len) => {
                let nested = reader.read_len_delimited()?;
                if let Some((entry_name, entry)) = parse_nested_map_entry(nested)? {
                    map_entries.insert(entry_name, entry);
                }
            }
            (8, WireType::Len) => {
                // OneofDescriptorProto: name = 1
                let decl = reader.read_len_delimited()?;
                let mut o = Reader::new(decl);
                let mut oneof_name = String::new();
                while !o.is_empty() {
                    let (n, w) = o.read_tag()?;
                    if n == 1 && w == WireType::Len {
                        oneof_name = read_string(&mut o)?;
                    } else {
                        o.skip(w)?;
                    }
                }
                oneofs.push(oneof_name);
            }
            (_, w) => reader.skip(w)?,
        }
    }

    let fields = raw_fields
        .into_iter()
        .map(|raw| interpret_field(raw, &name, &map_entries))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(MessageDescriptor {
        name,
        fields,
        oneofs,
    })
}

#[cfg(test)]
mod tests;
