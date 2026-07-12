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

    let mut fields = raw_fields
        .into_iter()
        .map(|raw| interpret_field(raw, &map_entries, &name))
        .collect::<Result<Vec<_>, _>>()?;

    // Drop any `oneof_index` that points past the declared groups: a malformed
    // descriptor could otherwise trigger an out-of-bounds panic when the
    // encoder or `oneofs()` indexes the group table.
    for field in &mut fields {
        if field
            .oneof_index
            .is_some_and(|idx| idx as usize >= oneofs.len())
        {
            field.oneof_index = None;
        }
    }

    Ok(MessageDescriptor {
        name,
        fields,
        oneofs,
    })
}

// FieldDescriptorProto.Label
const LABEL_REPEATED: i64 = 3;
// Well-known types surfaced as native Python objects (datetime / timedelta).
const TIMESTAMP_FULL_NAME: &str = ".google.protobuf.Timestamp";
const DURATION_FULL_NAME: &str = ".google.protobuf.Duration";
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

/// `.pkg.Outer.Inner` -> `Inner`. Used only to key the synthetic map-entry
/// table, whose entries are stored under their unqualified `DescriptorProto`
/// name.
fn short_name(full: &str) -> String {
    full.rsplit('.').next().unwrap_or(full).to_string()
}

/// `.pkg.Outer.Inner` -> `pkg.Outer.Inner`. The name we hand to Python for
/// linking: it keeps the full path (including any enclosing messages) so nested
/// types resolve unambiguously, dropping only protobuf's leading dot.
fn full_name(full: &str) -> String {
    full.strip_prefix('.').unwrap_or(full).to_string()
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
                        TYPE_MESSAGE => match raw.type_name.as_deref() {
                            Some(TIMESTAMP_FULL_NAME) => MapValue::Timestamp,
                            Some(DURATION_FULL_NAME) => MapValue::Duration,
                            _ => MapValue::Message,
                        },
                        other => MapValue::Scalar(
                            scalar_from_proto_type(other)
                                .ok_or(ParseError::UnsupportedType(other as i32))?,
                        ),
                    });
                    value_type_name = match value {
                        Some(MapValue::Timestamp | MapValue::Duration) => None,
                        _ => raw.type_name.as_deref().map(full_name),
                    };
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
    map_entries: &HashMap<String, MapEntry>,
    msg_name: &str,
) -> Result<FieldDescriptor, ParseError> {
    let type_code = raw.type_code.ok_or(ParseError::MissingType)?;
    let is_repeated = raw.label == LABEL_REPEATED;

    // Detect maps: a repeated message whose type is a nested map_entry. The
    // synthetic entry is nested in *this* message, so its full name ends in
    // `.<msg_name>.<EntryName>`. Checking the parent segment guards against a
    // real sibling type that merely shares the entry's short name.
    if is_repeated && type_code == TYPE_MESSAGE {
        if let Some(full) = &raw.type_name {
            let parent_is_this = full.rsplit('.').nth(1) == Some(msg_name);
            if parent_is_this {
                if let Some(entry) = map_entries.get(&short_name(full)) {
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
    }

    let (kind, type_name) = match type_code {
        TYPE_ENUM => (FieldKind::Enum, raw.type_name.as_deref().map(full_name)),
        TYPE_MESSAGE => match raw.type_name.as_deref() {
            // Native well-known types: no Python class to link (type_name None).
            Some(TIMESTAMP_FULL_NAME) => (FieldKind::Timestamp, None),
            Some(DURATION_FULL_NAME) => (FieldKind::Duration, None),
            _ => (FieldKind::Message, raw.type_name.as_deref().map(full_name)),
        },
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
    } else if raw.proto3_optional
        || matches!(
            kind,
            FieldKind::Message | FieldKind::Timestamp | FieldKind::Duration
        )
        || real_oneof.is_some()
    {
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

#[cfg(test)]
mod tests;
