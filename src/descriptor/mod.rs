//! The compiled, internal description of a message that the encoder and
//! decoder walk over.
//!
//! This is *not* the on-the-wire `FileDescriptorProto` — it is the digested
//! form we build once (at import time) and then reuse for every encode/decode.
//! It is deliberately free of any Python types so it can be unit-tested on its
//! own. Resolved references to other Python classes (for enum/message fields)
//! live separately, in the `Descriptor` wrapper, so this stays pure.

use crate::wire::WireType;

/// Maximum message nesting the codec will follow, on both encode and decode
/// (matches the default recursion limit of google's implementations). Guards
/// against stack exhaustion from adversarially nested input, and on encode it
/// also catches reference cycles between Python objects.
pub const MAX_DEPTH: usize = 100;

/// A protobuf scalar (leaf) value type. Mirrors the `Scalar.*` aliases exposed
/// on the Python side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarType {
    Double,
    Float,
    Int32,
    Int64,
    UInt32,
    UInt64,
    SInt32,
    SInt64,
    Fixed32,
    Fixed64,
    SFixed32,
    SFixed64,
    Bool,
    String,
    Bytes,
}

impl ScalarType {
    /// The wire type used to encode a single value of this scalar.
    pub fn wire_type(self) -> WireType {
        match self {
            ScalarType::Double | ScalarType::Fixed64 | ScalarType::SFixed64 => WireType::I64,
            ScalarType::Float | ScalarType::Fixed32 | ScalarType::SFixed32 => WireType::I32,
            ScalarType::String | ScalarType::Bytes => WireType::Len,
            _ => WireType::Varint,
        }
    }

    /// Whether this scalar is a numeric type eligible for packed encoding when
    /// repeated (everything except length-delimited string/bytes).
    pub fn is_packable(self) -> bool {
        !matches!(self, ScalarType::String | ScalarType::Bytes)
    }
}

/// The cardinality of a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// A plain proto3 field with no explicit presence: the default value is
    /// indistinguishable from "unset" and is not serialized.
    Single,
    /// A field with explicit presence (`optional`, a message field, or a real
    /// `oneof` member). Represented as `T | None` in Python.
    Optional,
    /// A `repeated` field, represented as `list[T]` in Python.
    Repeated,
}

/// The value type of a map's value slot.
#[derive(Debug, Clone, PartialEq)]
pub enum MapValue {
    Scalar(ScalarType),
    Enum,
    Message,
    /// `google.protobuf.Timestamp`, surfaced as a Python `datetime`.
    Timestamp,
    /// `google.protobuf.Duration`, surfaced as a Python `timedelta`.
    Duration,
}

/// What kind of value a field holds (independent of its cardinality).
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Scalar(ScalarType),
    /// A proto enum. Encoded as an `int32` varint; the Python side is an
    /// `IntEnum` subclass resolved at link time.
    Enum,
    /// A nested message. Encoded length-delimited; recurses through the child's
    /// own compiled descriptor.
    Message,
    /// A `map<K, V>` field. On the wire it is a repeated entry message with
    /// `key` = 1 and `value` = 2.
    Map {
        key: ScalarType,
        value: MapValue,
    },
    /// `google.protobuf.Timestamp`, surfaced as a Python `datetime`.
    Timestamp,
    /// `google.protobuf.Duration`, surfaced as a Python `timedelta`.
    Duration,
}

/// One field of a message.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDescriptor {
    /// Protobuf field number (the tag).
    pub number: u32,
    /// The Python attribute name on the dataclass instance.
    pub name: String,
    pub kind: FieldKind,
    pub label: Label,
    /// Fully-qualified proto name (minus the leading dot) for enum/message
    /// fields and a map's message/enum value, e.g. `pkg.Outer.Inner`. Used to
    /// resolve the Python class at link time; the full path lets nested types
    /// resolve unambiguously.
    pub type_name: Option<String>,
    /// Index into `MessageDescriptor::oneofs` if this is a real oneof member.
    pub oneof_index: Option<u32>,
}

impl FieldDescriptor {
    /// A convenience for the common scalar case (used in tests).
    #[cfg(test)]
    pub fn scalar(number: u32, name: &str, scalar: ScalarType, label: Label) -> FieldDescriptor {
        FieldDescriptor {
            number,
            name: name.to_string(),
            kind: FieldKind::Scalar(scalar),
            label,
            type_name: None,
            oneof_index: None,
        }
    }
}

/// A whole message type.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageDescriptor {
    pub name: String,
    pub fields: Vec<FieldDescriptor>,
    /// Names of the real (non-synthetic) oneof groups, index-aligned with
    /// `FieldDescriptor::oneof_index`.
    pub oneofs: Vec<String>,
}

impl MessageDescriptor {
    /// Look up a field by its wire number (used by the decoder).
    pub fn field_by_number(&self, number: u32) -> Option<&FieldDescriptor> {
        self.fields.iter().find(|f| f.number == number)
    }
}

#[cfg(test)]
mod tests;
