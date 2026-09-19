//! Minimal cursor for carrier-owned Kotlin protobuf messages.
//!
//! This cursor deliberately exposes only wire mechanics. Schema validation and semantic
//! interpretation stay in the declaration decoder that owns each message kind.

/// A protobuf wire-format cursor over a message body.
pub(crate) struct Pb<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Pb<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { b: bytes, i: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.i
    }

    pub(crate) fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.b.get(self.i)?;
            self.i += 1;
            value |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }

    pub(crate) fn bytes(&mut self, length: usize) -> Option<&'a [u8]> {
        let bytes = self.b.get(self.i..self.i.checked_add(length)?)?;
        self.i += length;
        Some(bytes)
    }

    pub(crate) fn at_end(&self) -> bool {
        self.i >= self.b.len()
    }

    /// Skip a field value by wire type; returns `None` for malformed or unsupported input.
    pub(crate) fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.bytes(8)?;
            }
            2 => {
                let length = self.varint()? as usize;
                self.bytes(length)?;
            }
            5 => {
                self.bytes(4)?;
            }
            _ => return None,
        }
        Some(())
    }
}

/// Read a packed repeated integer field atomically.
pub(crate) fn packed_varints(body: &[u8]) -> Option<Vec<u64>> {
    let mut protobuf = Pb::new(body);
    let mut values = Vec::new();
    while !protobuf.at_end() {
        values.push(protobuf.varint()?);
    }
    Some(values)
}
