//! The `type_parameter` sub-messages of a metadata declaration, read off the wire.

use crate::jvm::metadata::Pb;

/// Collect a message's repeated `type_parameter` sub-message bodies. The field number differs by
/// carrier — callers pass the schema-owned field for their declaration kind.
pub(super) fn type_param_bodies(body: &[u8], field: u64) -> Vec<&[u8]> {
    let mut pb = Pb::new(body);
    let mut out = Vec::new();
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (f, 2) if f == field => {
                let Some(n) = pb.varint() else { break };
                let Some(b) = pb.bytes(n as usize) else { break };
                out.push(b);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    out
}
