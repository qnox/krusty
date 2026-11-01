//! The typealias a declared result type was spelled through.

use super::string_table::{resolve_class_name, Rec};
use super::type_table_entry;
use crate::metadata::decode::{parse_type_node, Pb};
use crate::types::{type_name, TypeName};

/// The alias a declaration's result `Type` abbreviates: its `abbreviated_type` (field 13 inline,
/// or a type-table id in field 14) names the alias by `type_alias_name` (field 12). kotlinc
/// carries that abbreviation on the type a call or read of the declaration produces, so an
/// inferred result reached through it records the alias in its own `@Metadata`.
pub(super) fn result_alias(
    resolved: Option<(&[u8], bool)>,
    type_table: Option<&[u8]>,
    records: &[Rec],
    d2: &[String],
) -> Option<TypeName> {
    let (body, _) = resolved?;
    let mut message = Pb::new(body);
    let mut abbreviated = None;
    while !message.at_end() {
        let tag = message.varint()?;
        match (tag >> 3, tag & 7) {
            (13, 2) => {
                let length = message.varint()? as usize;
                abbreviated = Some(message.bytes(length)?);
            }
            (14, 0) => {
                let id = message.varint()? as usize;
                abbreviated = Some(type_table_entry(type_table?, id)?.0);
            }
            (_, wire) => message.skip(wire)?,
        }
    }
    let alias = parse_type_node(abbreviated?)?.type_alias_id?;
    Some(type_name(&resolve_class_name(records, d2, alias as usize)?))
}
