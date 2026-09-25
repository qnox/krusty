//! Minimal protobuf wire-format writer — enough to serialize the Kotlin metadata `Package`/
//! `Function` messages (`@kotlin.Metadata.d1`). Proto2 semantics: we only write fields that are set.
//!
//! Wire format: each field is `tag = (field_number << 3) | wire_type` (a varint), followed by the
//! value. Wire types used here: 0 = varint, 1 = fixed64, 2 = length-delimited (bytes / nested
//! message), and 5 = fixed32.

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

    pub fn field_fixed32(&mut self, field: u32, v: u32) {
        self.tag(field, 5);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn field_fixed64(&mut self, field: u32, v: u64) {
        self.tag(field, 1);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// `field: bytes/string` (wire type 2).
    pub fn field_bytes(&mut self, field: u32, b: &[u8]) {
        self.tag(field, 2);
        self.varint(b.len() as u64);
        self.buf.extend_from_slice(b);
    }

    /// `field: <message>` (wire type 2, length-delimited). The message is embedded in
    /// [`Pb::canonical`] order.
    pub fn field_message(&mut self, field: u32, msg: &Pb) {
        self.field_bytes(field, &msg.canonical().buf);
    }

    /// This message with its fields in field-number order, repeated fields keeping their relative
    /// order. That is the order protoc-generated `writeTo` serializes, which is how kotlinc writes
    /// every metadata message; builders append in whatever order their strings must intern.
    pub fn canonical(&self) -> Pb {
        let mut fields = Vec::new();
        let mut at = 0;
        while at < self.buf.len() {
            let start = at;
            let tag = read_varint(&self.buf, &mut at);
            match tag & 0x7 {
                0 => {
                    read_varint(&self.buf, &mut at);
                }
                1 => at += 8,
                2 => {
                    let length = read_varint(&self.buf, &mut at) as usize;
                    at += length;
                }
                5 => at += 4,
                wire => unreachable!("the metadata writer never emits wire type {wire}"),
            }
            fields.push((tag >> 3, &self.buf[start..at]));
        }
        fields.sort_by_key(|(field, _)| *field);
        Pb {
            buf: fields
                .into_iter()
                .flat_map(|(_, bytes)| bytes)
                .copied()
                .collect(),
        }
    }

    /// One element of a `repeated <message>` field (emit the tag+message once per element).
    pub fn repeated_message(&mut self, field: u32, msg: &Pb) {
        self.field_message(field, msg);
    }
    pub fn repeated_varint(&mut self, field: u32, v: u64) {
        self.field_varint(field, v);
    }
}

fn read_varint(buf: &[u8], at: &mut usize) -> u64 {
    let mut value = 0;
    let mut shift = 0;
    loop {
        let byte = buf[*at];
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
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

    #[test]
    fn canonical_orders_fields_by_number_and_keeps_repeated_order() {
        let mut inner = Pb::new();
        inner.field_varint(6, 1);
        inner.field_varint(1, 1);
        let mut outer = Pb::new();
        outer.repeated_varint(4, 2);
        outer.field_varint(3, 7);
        outer.repeated_varint(4, 1);
        outer.field_message(2, &inner);
        assert_eq!(
            outer.canonical().as_bytes(),
            &[0x12, 0x04, 0x08, 0x01, 0x30, 0x01, 0x18, 0x07, 0x20, 0x02, 0x20, 0x01]
        );
    }
}
