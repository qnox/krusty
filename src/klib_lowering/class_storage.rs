//! Declared instance storage authenticated in member order.

use super::class_headers::ClassMemberHeader;
use super::decline::KlibBodyDeclineReason;
use super::klib_types::semantic_type;
use crate::fx_hash::HashSet;
use crate::metadata::klib_ir::tree::{KlibIrField, KlibIrMember};
use crate::metadata::klib_ir::{KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Visibility;

pub(super) fn member_field(member: &KlibIrMember) -> Option<&KlibIrField> {
    match member {
        KlibIrMember::Field(field) => Some(field),
        KlibIrMember::Property(property) => property.backing_field.as_ref(),
        _ => None,
    }
}

/// Incremental authentication lets constructors preserve field/initializer diagnostic order.
pub(super) fn declared_field<'a>(
    header: &ClassMemberHeader<'_, 'a>,
    field: &'a KlibIrField,
    identities: &mut HashSet<&'a KlibIrSymbol>,
) -> Result<crate::libraries::KlibStoredField, KlibBodyDeclineReason> {
    let flags =
        field
            .instance_storage_flags()
            .ok_or(KlibBodyDeclineReason::UnsupportedOperation(
                "an external, static or unknown field storage flag",
            ))?;
    if !matches!(
        field.base.symbol.kind,
        KlibIrSymbolKind::Field | KlibIrSymbolKind::StandaloneField
    ) || !identities.insert(&field.base.symbol)
    {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "a class field has an invalid or repeated storage identity".to_owned(),
        ));
    }
    Ok(crate::libraries::KlibStoredField {
        name: field.name.clone(),
        ty: semantic_type(header.arena, &header.types, field.ty)?,
        final_: flags.is_final,
        private: flags.visibility == Visibility::Private,
    })
}
