//! `SerializationPluginMetadataExtensions.propertiesNamesInProgramOrder`, the class metadata field
//! in which kotlinx.serialization's plugin lists an open or abstract serializable class's own
//! serial elements in declaration order.
//!
//! A subclass compiled against the class generates its serializer from the class's metadata alone,
//! and the metadata's own property records are not in declaration order, so this list is the only
//! record of where each inherited element sits on the wire.

use super::*;

/// The field number the serialization plugin registers on the `Class` message.
const PROPERTIES_NAMES_IN_PROGRAM_ORDER: u64 = 18000;

/// The element names the class lists, in program order; empty when the class lists none.
pub(super) fn serial_element_names(ctx: &MetaCtx) -> Box<[Box<str>]> {
    let mut names = Vec::new();
    let push = |id: u64, names: &mut Vec<Box<str>>| {
        if let Some(name) = resolve_string(ctx.records, ctx.d2, id as usize) {
            names.push(name.into_boxed_str());
        }
    };
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (PROPERTIES_NAMES_IN_PROGRAM_ORDER, 0) => {
                let Some(id) = pb.varint() else { break };
                push(id, &mut names);
            }
            (PROPERTIES_NAMES_IN_PROGRAM_ORDER, 2) => {
                let Some(length) = pb.varint() else { break };
                let Some(body) = pb.bytes(length as usize) else {
                    break;
                };
                for id in packed_varints(body).unwrap_or_default() {
                    push(id, &mut names);
                }
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    names.into_boxed_slice()
}
