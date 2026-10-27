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
const CLASS_PROPERTY: u64 = 10;

/// One listed element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SerialElementName {
    pub name: Box<str>,
    /// How the element's property type is spelled, with the type-use annotations at each of its
    /// positions (a serializer of its own, `@Contextual`, …). `None` when this decoder could not
    /// read them all: the type alone does not say how the element is serialized.
    pub type_spelling: Option<crate::spelling::Spelled>,
}

/// The elements the class lists, in wire order; empty when the class lists none.
pub(super) fn serial_element_names(ctx: &MetaCtx) -> Box<[SerialElementName]> {
    let mut ids = Vec::new();
    let mut spellings = crate::fx_hash::HashMap::default();
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
                if let Some((name, spelling)) = property_type_spelling(ctx, body) {
                    spellings.insert(name, spelling);
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
                type_spelling: spellings.get(&id).cloned().flatten(),
                name: name.into_boxed_str(),
            })
        })
        .collect()
}

/// A `Property` message's name id and its return type's spelling; `None` for the spelling when
/// the type is not one this decoder reads fully.
fn property_type_spelling(
    ctx: &MetaCtx,
    body: &[u8],
) -> Option<(u64, Option<crate::spelling::Spelled>)> {
    let mut pb = Pb::new(body);
    let mut name = None;
    let mut spelling = None;
    while !pb.at_end() {
        let tag = pb.varint()?;
        match (tag >> 3, tag & 7) {
            (2, 0) => name = Some(pb.varint()?),
            (3, 2) => {
                let length = pb.varint()? as usize;
                let body = pb.bytes(length)?;
                spelling = readable(body)
                    .then(|| type_aliases::type_spelling(body, ctx.records, ctx.d2))
                    .flatten();
            }
            // A return type in the type table: this decoder does not follow it.
            (9, 0) => {
                pb.varint()?;
                spelling = None;
            }
            (_, wire) => pb.skip(wire)?,
        }
    }
    Some((name?, spelling))
}

/// Whether a `Type`'s spelling is read fully: neither it nor a type inside it is flexible, has an
/// outer type, or lives in the type table.
fn readable(body: &[u8]) -> bool {
    let Some(node) = parse_type_node(body) else {
        return false;
    };
    node.flexible_upper_bound_id.is_none()
        && node.outer_type_id.is_none()
        && node.flexible_upper_bound.is_none()
        && node.outer_type.is_none()
        && node.arguments.iter().all(|argument| match argument {
            ParsedTypeArgument::Inline(body, _) => readable(body),
            ParsedTypeArgument::Table(..) => false,
            ParsedTypeArgument::Star => true,
        })
}
