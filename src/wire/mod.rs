//! Pure protobuf wire-format primitives.
//!
//! This module knows nothing about Python or descriptors. It only reads and
//! writes the low-level building blocks of the protobuf binary encoding:
//! varints, zig-zag integers, fixed-width numbers, length-delimited chunks and
//! field tags. Everything here is exercised directly by `cargo test`.

/// Protobuf wire types (the low 3 bits of a field tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    Varint = 0,
    I64 = 1,
    Len = 2,
    I32 = 5,
}

impl WireType {
    pub fn from_u64(value: u64) -> Option<WireType> {
        match value {
            0 => Some(WireType::Varint),
            1 => Some(WireType::I64),
            2 => Some(WireType::Len),
            5 => Some(WireType::I32),
            _ => None,
        }
    }
}

/// Error raised while decoding malformed wire data.
#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    /// Ran off the end of the buffer while reading.
    UnexpectedEof,
    /// A varint was longer than 10 bytes (would overflow u64).
    VarintOverflow,
    /// The low 3 bits of a tag were not a recognised wire type.
    InvalidWireType(u64),
    /// A length-delimited field claimed more bytes than remain.
    InvalidLength,
    /// A tag's field number was 0 or beyond the protobuf maximum (2^29 - 1).
    InvalidFieldNumber(u64),
}

/// The largest legal protobuf field number (field numbers are 29-bit).
pub const MAX_FIELD_NUMBER: u64 = (1 << 29) - 1;

/// Cursor over an input buffer that reads wire primitives.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    /// Whether the whole buffer has been consumed.
    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Current byte offset into the buffer.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// The raw bytes consumed since `start` (a value previously returned by
    /// [`Reader::pos`]). Used to preserve unknown fields verbatim.
    pub fn raw_since(&self, start: usize) -> &'a [u8] {
        &self.buf[start..self.pos]
    }

    /// Read a base-128 varint as a raw u64.
    pub fn read_varint(&mut self) -> Result<u64, WireError> {
        let mut result: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = self.next_byte()?;
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
        // A 10th continuation byte would push past 64 bits.
        Err(WireError::VarintOverflow)
    }

    /// Read a field tag, returning `(field_number, wire_type)`.
    pub fn read_tag(&mut self) -> Result<(u32, WireType), WireError> {
        let key = self.read_varint()?;
        let wire = WireType::from_u64(key & 0x7).ok_or(WireError::InvalidWireType(key & 0x7))?;
        // `key >> 3` may exceed 32 bits for a malformed tag; validate before the
        // cast so a huge field number can't silently truncate onto a real one.
        let field = key >> 3;
        if field == 0 || field > MAX_FIELD_NUMBER {
            return Err(WireError::InvalidFieldNumber(field));
        }
        Ok((field as u32, wire))
    }

    /// Read a little-endian fixed 32-bit value.
    pub fn read_fixed32(&mut self) -> Result<u32, WireError> {
        let mut bytes = [0u8; 4];
        for slot in bytes.iter_mut() {
            *slot = self.next_byte()?;
        }
        Ok(u32::from_le_bytes(bytes))
    }

    /// Read a little-endian fixed 64-bit value.
    pub fn read_fixed64(&mut self) -> Result<u64, WireError> {
        let mut bytes = [0u8; 8];
        for slot in bytes.iter_mut() {
            *slot = self.next_byte()?;
        }
        Ok(u64::from_le_bytes(bytes))
    }

    /// Read a length-delimited chunk, returning a borrowed slice.
    pub fn read_len_delimited(&mut self) -> Result<&'a [u8], WireError> {
        let len = self.read_varint()? as usize;
        let end = self.pos.checked_add(len).ok_or(WireError::InvalidLength)?;
        if end > self.buf.len() {
            return Err(WireError::InvalidLength);
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Skip a field of the given wire type (used for unknown fields).
    pub fn skip(&mut self, wire: WireType) -> Result<(), WireError> {
        match wire {
            WireType::Varint => {
                self.read_varint()?;
            }
            WireType::I64 => {
                self.read_fixed64()?;
            }
            WireType::I32 => {
                self.read_fixed32()?;
            }
            WireType::Len => {
                self.read_len_delimited()?;
            }
        }
        Ok(())
    }

    fn next_byte(&mut self) -> Result<u8, WireError> {
        let b = *self.buf.get(self.pos).ok_or(WireError::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }
}

/// Append a base-128 varint to `buf`.
pub fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

/// Append a field tag built from a field number and wire type.
pub fn write_tag(buf: &mut Vec<u8>, field: u32, wire: WireType) {
    write_varint(buf, ((field as u64) << 3) | (wire as u64));
}

/// Append a little-endian fixed 32-bit value.
pub fn write_fixed32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Append a little-endian fixed 64-bit value.
pub fn write_fixed64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Append a length-delimited chunk (length varint followed by bytes).
pub fn write_len_delimited(buf: &mut Vec<u8>, data: &[u8]) {
    write_varint(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

/// Zig-zag encode a signed 32-bit integer (for `sint32`).
pub fn zigzag_encode32(value: i32) -> u32 {
    ((value << 1) ^ (value >> 31)) as u32
}

/// Zig-zag decode to a signed 32-bit integer.
pub fn zigzag_decode32(value: u32) -> i32 {
    ((value >> 1) as i32) ^ -((value & 1) as i32)
}

/// Zig-zag encode a signed 64-bit integer (for `sint64`).
pub fn zigzag_encode64(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

/// Zig-zag decode to a signed 64-bit integer.
pub fn zigzag_decode64(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

#[cfg(test)]
mod tests;
