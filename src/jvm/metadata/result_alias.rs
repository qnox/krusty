//! The typealias application a declared result type was spelled through.

use super::type_table_entry;
use crate::metadata::decode::Pb;

/// The `abbreviated_type` of a declaration's result `Type` (field 13 inline, or a type-table id in
/// field 14), with its nullability: the alias applied to its as-written arguments. Decoded in the
/// declaration's type-parameter context it becomes a
/// [`DeclaredResultAlias`](crate::libraries::DeclaredResultAlias).
pub(super) fn abbreviated_type<'a>(
    resolved: Option<(&'a [u8], bool)>,
    type_table: Option<&'a [u8]>,
) -> Option<(&'a [u8], bool)> {
    let (body, _) = resolved?;
    let mut message = Pb::new(body);
    let mut abbreviated = None;
    while !message.at_end() {
        let tag = message.varint()?;
        match (tag >> 3, tag & 7) {
            (13, 2) => {
                let length = message.varint()? as usize;
                abbreviated = Some((message.bytes(length)?, false));
            }
            (14, 0) => {
                let id = message.varint()? as usize;
                abbreviated = Some(type_table_entry(type_table?, id)?);
            }
            (_, wire) => message.skip(wire)?,
        }
    }
    abbreviated
}

/// The [`DeclaredResultAlias`](crate::libraries::DeclaredResultAlias) a declaration's result
/// `Type` carries, decoded in the declaration's type-parameter `context`.
pub(super) fn declared(
    resolved: Option<(&[u8], bool)>,
    type_table: Option<&[u8]>,
    records: &[super::string_table::Rec],
    d2: &[String],
    context: Option<&super::TypeParameterContext>,
) -> Option<crate::libraries::DeclaredResultAlias> {
    let abbreviated = abbreviated_type(resolved, type_table)?;
    let application =
        super::decode_metadata_type_ref(Some(abbreviated), type_table, records, d2, context)?;
    crate::libraries::DeclaredResultAlias::from_application(application)
}
