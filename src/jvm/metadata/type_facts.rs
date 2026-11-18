//! The facts one Kotlin metadata `Type` message states about itself: nullability, suspend-function
//! identity and the type-use annotations written on it.

use super::string_table::{resolve_class_name, Rec};
use crate::metadata::decode::{parse_type_node, Pb};

/// The Compose runtime's type annotation on a function type, whose values are compiled with the
/// composer and `$changed` parameters after their own.
pub(super) const COMPOSABLE_ANNOTATION: &str = "androidx/compose/runtime/Composable";

/// The declaration facts carried directly by one Kotlin metadata `Type` message.
///
/// Nullability and suspend-function identity live in the same protobuf node. Decode them in one walk
/// so a value-parameter consumer cannot accidentally read one from an inline type and the other from
/// a type-table entry, or duplicate the wire parser as new type flags are added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedTypeFacts {
    pub(super) nullable: bool,
    pub(super) suspend_fun: bool,
}

pub(super) fn parse_type_facts(body: &[u8]) -> ParsedTypeFacts {
    let mut pb = Pb::new(body);
    let mut facts = ParsedTypeFacts::default();
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            // `Type.flags` bit zero is `SUSPEND_TYPE`. A suspend function type otherwise has the
            // same CPS-erased `FunctionN+1<..., Continuation<R>, Any?>` signature as an ordinary
            // continuation-taking function, so this bit is the semantic discriminator.
            (1, 0) => facts.suspend_fun = pb.varint().is_some_and(|v| v & 1 != 0),
            (3, 0) => facts.nullable = pb.varint().is_some_and(|v| v != 0),
            (_, w) => {
                if pb.skip(w).is_none() {
                    break;
                }
            }
        }
    }
    facts
}

/// Whether a `Type` message is nullable (`Type.nullable = 3`, a varint bool). Kept as the small
/// query used throughout the decoder; the wire interpretation itself remains centralized above.
pub(super) fn parse_type_nullable(body: &[u8]) -> bool {
    parse_type_facts(body).nullable
}

/// Whether the `Type` message `body` carries a type-use application of the class `annotation`.
pub(super) fn is_annotated_with(
    body: &[u8],
    records: &[Rec],
    d2: &[String],
    annotation: &str,
) -> bool {
    parse_type_node(body).is_some_and(|node| {
        node.annotations.into_iter().any(|applied| {
            applied
                .class_id()
                .and_then(|id| resolve_class_name(records, d2, id as usize))
                .is_some_and(|name| name == annotation)
        })
    })
}
