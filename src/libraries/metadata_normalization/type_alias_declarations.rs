//! Kotlin type aliases normalized into target-neutral expansion templates.

use super::type_signatures::EnclosingBounds;
use crate::libraries::{type_alias_target_classifier, AliasExpansion};
use crate::metadata::semantic::{semantic_bounds, semantic_ty, KotlinTypeAlias};
use crate::types::{type_name_child, type_name_nested_child, ClassifierDeclarationName, TypeName};

/// Publish `alias` under `owner`, retaining the complete expansion template that source type
/// arguments are substituted into. The expanded classifier is a decoded metadata fact; no target
/// provider lookup participates in choosing it.
pub(crate) fn declared_type_alias(
    owner: TypeName,
    alias: &KotlinTypeAlias,
    inherited: &EnclosingBounds,
) -> Result<AliasExpansion, &'static str> {
    declared_type_alias_with_identity(
        type_name_child(owner, &alias.name),
        ClassifierDeclarationName::TopLevel {
            package: owner,
            name: alias.name.as_str().into(),
        },
        alias,
        inherited,
    )
}

/// Publish a type alias in a classifier's nested-name namespace.
pub(crate) fn declared_nested_type_alias(
    owner: TypeName,
    alias: &KotlinTypeAlias,
    inherited: &EnclosingBounds,
) -> Result<AliasExpansion, &'static str> {
    declared_type_alias_with_identity(
        type_name_nested_child(owner, &alias.name),
        ClassifierDeclarationName::Nested {
            owner,
            name: alias.name.as_str().into(),
        },
        alias,
        inherited,
    )
}

fn declared_type_alias_with_identity(
    identity: TypeName,
    declaration_name: ClassifierDeclarationName,
    alias: &KotlinTypeAlias,
    inherited: &EnclosingBounds,
) -> Result<AliasExpansion, &'static str> {
    let bounds = semantic_bounds(&alias.formals, inherited);
    let expansion = semantic_ty(&alias.expansion, &bounds);
    let target = type_alias_target_classifier(expansion)
        .ok_or("expanded type has no semantic classifier identity")?;
    Ok(AliasExpansion {
        identity,
        declaration_name: Some(std::sync::Arc::new(declaration_name)),
        target,
        formals: alias
            .formals
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        expansion,
        expansion_spelling: alias.expansion_spelling.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::semantic::KotlinType;
    use crate::types::{type_name, ClassifierDeclarationName, Ty, Visibility};

    fn alias(name: &str) -> KotlinTypeAlias {
        KotlinTypeAlias {
            name: name.into(),
            formals: Vec::new(),
            expansion: KotlinType::class("alias_target/Real"),
            expansion_spelling: crate::spelling::Spelled::default(),
            visibility: Visibility::Public,
        }
    }

    #[test]
    fn alias_declaration_name_is_independent_of_flattened_identity_and_target() {
        let package = type_name("alias_owned_snapshot");
        let owner = type_name_child(package, "Literal");
        let bounds = EnclosingBounds::default();
        let top = declared_type_alias(package, &alias("Literal$Alias"), &bounds).unwrap();
        let nested = declared_nested_type_alias(owner, &alias("Alias"), &bounds).unwrap();
        let identity = type_name_child(package, "Literal$Alias");
        let target = type_name("alias_target/Real");
        assert_eq!(
            top,
            AliasExpansion {
                identity,
                declaration_name: Some(std::sync::Arc::new(ClassifierDeclarationName::TopLevel {
                    package,
                    name: "Literal$Alias".into(),
                })),
                target,
                formals: Vec::new(),
                expansion: Ty::obj_name(target),
                expansion_spelling: crate::spelling::Spelled::default(),
            }
        );
        assert_eq!(
            nested,
            AliasExpansion {
                identity,
                declaration_name: Some(std::sync::Arc::new(ClassifierDeclarationName::Nested {
                    owner,
                    name: "Alias".into(),
                })),
                target,
                formals: Vec::new(),
                expansion: Ty::obj_name(target),
                expansion_spelling: crate::spelling::Spelled::default(),
            }
        );
    }
}
