//! Temporary stable-path reads for checkers constructed before signature finalization.
//!
//! The sole caller is [`super::stable_path::StablePathRead`] when `Checker::resolved_index` is
//! absent. Delete this module and that branch once `analyze_source`, the public `check_file*`
//! entry points, and Pass-1 inference construct their checkers from a finalized declaration index.

use super::{CheckerModuleSymbols, Decl, DeclId, File, Ty, TypeName};

pub(super) fn top_level_ty(
    file: &File,
    file_index: u32,
    property: &crate::libraries::PropertyInfo,
) -> Option<Ty> {
    let (source_file, source_decl) = property.source_key?;
    if source_file != file_index {
        return None;
    }
    let Decl::Property(declaration) = file.decl(DeclId(source_decl)) else {
        return None;
    };
    if declaration.is_var
        || declaration.getter_declared
        || declaration.delegate.is_some()
        || declaration.is_external
        || declaration.is_expect
    {
        return None;
    }
    Some(property.ty)
}

pub(super) fn member_ty(
    module: &CheckerModuleSymbols<'_>,
    receiver: Ty,
    owner: TypeName,
    name: &str,
    owner_is_final: bool,
) -> Option<Ty> {
    let symbols = module.legacy_symbols()?;
    let (declaring_owner, property) = symbols.declared_member_prop(owner, name)?;
    if property.setter_name.is_some()
        || property.has_custom_getter
        || (property.is_open && !owner_is_final)
        || !property.context_params.is_empty()
    {
        return None;
    }
    // Keep the legacy generic-property instantiation identical to its ordinary member-read path.
    Some(
        symbols
            .applied_declared_member_prop_ty(receiver, declaring_owner, name, property.ty)
            .projection_read_ty(),
    )
}
