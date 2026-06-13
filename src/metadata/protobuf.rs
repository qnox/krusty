//! Minimal protobuf wire-format writer — enough to serialize the Kotlin metadata `Package`/
//! `Function` messages (`@kotlin.Metadata.d1`). Proto2 semantics: we only write fields that are set.
//!
//! Wire format: each field is `tag = (field_number << 3) | wire_type` (a varint), followed by the
//! value. Wire types used here: 0 = varint, 2 = length-delimited (bytes / nested message).

#[derive(Default, Clone)]
pub struct Pb {
    buf: Vec<u8>,
}

impl Pb {
    pub fn new() -> Pb {
        Pb { buf: Vec::new() }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Append a base-128 varint (unsigned LEB128).
    pub fn varint(&mut self, mut v: u64) {
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            self.buf.push(byte);
            if v == 0 {
                break;
            }
        }
    }

    fn tag(&mut self, field: u32, wire_type: u8) {
        self.varint(((field as u64) << 3) | wire_type as u64);
    }

    /// `field: int32/int64/bool/enum` (wire type 0).
    pub fn field_varint(&mut self, field: u32, v: u64) {
        self.tag(field, 0);
        self.varint(v);
    }

    /// `field: bytes/string` (wire type 2).
    pub fn field_bytes(&mut self, field: u32, b: &[u8]) {
        self.tag(field, 2);
        self.varint(b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    /// `field: <message>` (wire type 2, length-delimited).
    pub fn field_message(&mut self, field: u32, msg: &Pb) {
        self.field_bytes(field, &msg.buf);
    }

    /// One element of a `repeated <message>` field (emit the tag+message once per element).
    pub fn repeated_message(&mut self, field: u32, msg: &Pb) {
        self.field_message(field, msg);
    }
    pub fn repeated_varint(&mut self, field: u32, v: u64) {
        self.field_varint(field, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_varint_field() {
        // From the protobuf spec: message { field 1 = 150 } encodes as 08 96 01.
        let mut p = Pb::new();
        p.field_varint(1, 150);
        assert_eq!(p.as_bytes(), &[0x08, 0x96, 0x01]);
    }

    #[test]
    fn varint_small_and_boundaries() {
        let mut p = Pb::new();
        p.varint(0);
        p.varint(1);
        p.varint(127);
        p.varint(128);
        p.varint(300);
        assert_eq!(p.as_bytes(), &[0x00, 0x01, 0x7f, 0x80, 0x01, 0xac, 0x02]);
    }

    #[test]
    fn length_delimited_string() {
        // field 2, "testing" => 12 07 t e s t i n g
        let mut p = Pb::new();
        p.field_bytes(2, b"testing");
        let mut expect = vec![0x12, 0x07];
        expect.extend_from_slice(b"testing");
        assert_eq!(p.as_bytes(), &expect);
    }

    #[test]
    fn nested_message() {
        // outer { field 3 : inner { field 1 = 150 } } => 1a 03 08 96 01
        let mut inner = Pb::new();
        inner.field_varint(1, 150);
        let mut outer = Pb::new();
        outer.field_message(3, &inner);
        assert_eq!(outer.as_bytes(), &[0x1a, 0x03, 0x08, 0x96, 0x01]);
    }

    #[test]
    fn repeated_fields_concatenate() {
        let mut p = Pb::new();
        p.repeated_varint(4, 1);
        p.repeated_varint(4, 2);
        p.repeated_varint(4, 3);
        // tag for field 4 varint = (4<<3)|0 = 0x20
        assert_eq!(p.as_bytes(), &[0x20, 0x01, 0x20, 0x02, 0x20, 0x03]);
    }
}
