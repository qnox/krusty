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
    stored_field(field, identities, || {
        semantic_type(header.arena, &header.types, field.ty)
    })
}

/// Storage identity and flags, then `ty`. Callers resolve `ty` from the declaration that owns the
/// field; a public class and a same-library file-local class share this check.
pub(super) fn stored_field<'a>(
    field: &'a KlibIrField,
    identities: &mut HashSet<&'a KlibIrSymbol>,
    ty: impl FnOnce() -> Result<crate::types::Ty, KlibBodyDeclineReason>,
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
        ty: ty()?,
        final_: flags.is_final,
        private: flags.visibility == Visibility::Private,
    })
}

/// The declaration-owned instance fields, without opening their initializer expressions.
pub(super) struct ClassStoragePlan<'a> {
    pub(super) field_declarations: Vec<&'a KlibIrField>,
    pub(super) storage: crate::libraries::KlibClassStorage,
}

/// Authenticate fields in declared order. The caller visits the same members at that boundary,
/// preserving constructor-initializer diagnostics and evaluation order without making those
/// executable payloads part of the persisted storage declaration.
pub(super) fn authenticate<'a>(
    header: &ClassMemberHeader<'_, 'a>,
    superclass: crate::types::Ty,
    mut member: impl FnMut(
        &'a KlibIrMember,
        Option<(u32, &'a KlibIrField)>,
    ) -> Result<(), KlibBodyDeclineReason>,
) -> Result<ClassStoragePlan<'a>, KlibBodyDeclineReason> {
    let mut fields = Vec::new();
    let mut field_declarations = Vec::new();
    let mut identities = HashSet::default();
    let members: &'a [KlibIrMember] = &header.class.members;
    for declaration in members {
        let field = if let Some(field) = member_field(declaration) {
            let index = u32::try_from(fields.len()).expect("a field list fits u32");
            fields.push(declared_field(header, field, &mut identities)?);
            field_declarations.push(field);
            Some((index, field))
        } else {
            None
        };
        member(declaration, field)?;
    }
    Ok(ClassStoragePlan {
        field_declarations,
        storage: crate::libraries::KlibClassStorage {
            declaration: header.contract.clone(),
            superclass,
            fields,
        },
    })
}
