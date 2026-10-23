//! `SerializationPluginMetadataExtensions.propertiesNamesInProgramOrder`, the class metadata field
//! in which kotlinx.serialization's plugin lists an open or abstract serializable class's serial
//! elements in wire order: the ones it inherits first, then its own in declaration order.
//!
//! A subclass compiled against the class generates its serializer from the class's metadata alone,
//! and the metadata's own property records are not in declaration order, so this list is the only
//! record of where each inherited element sits on the wire.

use super::*;

/// The field number the serialization plugin registers on the `Class` message.
const PROPERTIES_NAMES_IN_PROGRAM_ORDER: u64 = 18000;
/// `Class.property`.
const CLASS_PROPERTY: u64 = 13;

/// One listed element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SerialElementName {
    pub name: Box<str>,
    /// The element's property type carries a type-use annotation somewhere (a serializer of its
    /// own, `@Contextual`, …), or this decoder could not tell: the type alone does not say how
    /// the element is serialized.
    pub annotated_type: bool,
}

/// The elements the class lists, in wire order; empty when the class lists none.
pub(super) fn serial_element_names(ctx: &MetaCtx) -> Box<[SerialElementName]> {
    let mut ids = Vec::new();
    let mut annotated = crate::fx_hash::HashSet::default();
    let mut pb = Pb::new(ctx.msg);
    while !pb.at_end() {
        let Some(tag) = pb.varint() else { break };
        match (tag >> 3, tag & 7) {
            (PROPERTIES_NAMES_IN_PROGRAM_ORDER, 0) => {
                let Some(id) = pb.varint() else { break };
                ids.push(id);
            }
            (PROPERTIES_NAMES_IN_PROGRAM_ORDER, 2) => {
                let Some(length) = pb.varint() else { break };
                let Some(body) = pb.bytes(length as usize) else {
                    break;
                };
                ids.extend(packed_varints(body).unwrap_or_default());
            }
            (CLASS_PROPERTY, 2) => {
                let Some(length) = pb.varint() else { break };
                let Some(body) = pb.bytes(length as usize) else {
                    break;
                };
                if let Some((name, true)) = property_type_annotated(body) {
                    annotated.insert(name);
                }
            }
            (_, wire) => {
                if pb.skip(wire).is_none() {
                    break;
                }
            }
        }
    }
    ids.into_iter()
        .filter_map(|id| {
            let name = resolve_string(ctx.records, ctx.d2, id as usize)?;
            Some(SerialElementName {
                annotated_type: annotated.contains(&id),
                name: name.into_boxed_str(),
            })
        })
        .collect()
}

/// A `Property` message's name id and whether its return type carries a type annotation.
fn property_type_annotated(body: &[u8]) -> Option<(u64, bool)> {
    let mut pb = Pb::new(body);
    let mut name = None;
    let mut annotated = false;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => name = Some(pb.varint()?),
            (3, 2) => {
                let length = pb.varint()? as usize;
                annotated = type_annotated(pb.bytes(length)?);
            }
            // A return type in the type table: this decoder does not follow it.
            (9, 0) => {
                pb.varint()?;
                annotated = true;
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    Some((name?, annotated))
}

/// Whether a `Type` or any type inside it carries an annotation. A type this decoder cannot read
/// fully counts as annotated.
fn type_annotated(body: &[u8]) -> bool {
    let Some(node) = parse_type_node(body) else {
        return true;
    };
    !node.annotations.is_empty()
        || node.flexible_upper_bound_id.is_some()
        || node.outer_type_id.is_some()
        || node.flexible_upper_bound.is_some_and(type_annotated)
        || node.outer_type.is_some_and(type_annotated)
        || node.arguments.iter().any(|argument| match argument {
            ParsedTypeArgument::Inline(body, _) => type_annotated(body),
            ParsedTypeArgument::Table(..) => true,
            ParsedTypeArgument::Star => false,
        })
}
