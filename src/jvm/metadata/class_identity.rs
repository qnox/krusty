//! Kotlin class identity fields decoded from the class metadata protobuf.

use super::*;

/// `Class.fq_name` (field 3) as a Kotlin qualified name. Metadata spells a class name with `/`
/// between package segments and `.` between classes (`lib/Outer.Nested`); the qualified name dots
/// both, and neither character can occur inside a JVM identifier.
pub(super) fn class_qualified_name(ctx: &MetaCtx) -> Option<String> {
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (3, 0) => {
                let id = pb.varint()?;
                return resolve_class_name(ctx.records, ctx.d2, id as usize)
                    .map(|name| name.replace('/', "."));
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    None
}

/// The simple name of a class's companion object (`Class.companion_object_name = 4`), e.g.
/// `Companion`. `None` if the class has no companion.
pub(super) fn companion_name(ctx: &MetaCtx) -> Option<String> {
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (4, 0) => {
                let id = pb.varint()?;
                return resolve_class_name(ctx.records, ctx.d2, id as usize);
            }
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    None
}
