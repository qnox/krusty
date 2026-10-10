//! Kotlin type aliases normalized into target-neutral expansion templates.

use super::type_signatures::EnclosingBounds;
use crate::libraries::{type_alias_target_classifier, AliasExpansion};
use crate::metadata::semantic::{semantic_bounds, semantic_ty, KotlinTypeAlias};
use crate::types::{type_name_child, TypeName};

/// Publish `alias` under `owner`, retaining the complete expansion template that source type
/// arguments are substituted into. The expanded classifier is a decoded metadata fact; no target
/// provider lookup participates in choosing it.
pub(crate) fn declared_type_alias(
    owner: TypeName,
    alias: &KotlinTypeAlias,
    inherited: &EnclosingBounds,
) -> Result<AliasExpansion, &'static str> {
    let bounds = semantic_bounds(&alias.formals, inherited);
    let expansion = semantic_ty(&alias.expansion, &bounds);
    let target = type_alias_target_classifier(expansion)
        .ok_or("expanded type has no semantic classifier identity")?;
    Ok(AliasExpansion {
        identity: type_name_child(owner, &alias.name),
        target,
        formals: alias
            .formals
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        expansion,
        // KLIB's common semantic decoder does not yet retain type-use spelling annotations.
        expansion_spelling: Default::default(),
    })
}
