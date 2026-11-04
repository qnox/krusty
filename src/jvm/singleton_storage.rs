//! JVM storage of an already-resolved Kotlin singleton classifier.
//!
//! Providers normalize declaration ownership into backend classifier facts. This operation maps
//! those facts to one physical JVM field; callers do not retry module, classpath, and spelling
//! representations independently.

use crate::backend::BackendClassifierSource;
use crate::libraries::TypeKind;
use crate::types::{type_name, ClassifierDeclarationName, TypeName};

pub(super) fn of(
    classifiers: &dyn BackendClassifierSource,
    classifier: TypeName,
) -> Option<(TypeName, String)> {
    if let Some(owner) = super::jvm_class_map::intrinsic_companion_jvm_class(classifier) {
        return Some((type_name(&owner), "INSTANCE".to_string()));
    }

    let declaration = classifiers.classifier(classifier)?;
    if declaration.kind != TypeKind::Object {
        return None;
    }
    if let ClassifierDeclarationName::Nested { owner: holder, .. } =
        declaration.declaration_name.as_ref()?
    {
        let holder_declaration = classifiers.classifier(*holder)?;
        if let Some((field, companion)) = holder_declaration.companion.as_ref() {
            if *companion == classifier {
                return Some((*holder, field.to_string()));
            }
        }
    }
    Some((classifier, "INSTANCE".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendModuleFacts, CheckedBackendClassifiers};
    use crate::libraries::{EmptySymbolSource, LibraryType};

    fn object(name: Option<ClassifierDeclarationName>) -> LibraryType {
        let mut shape = LibraryType::declaration_header();
        shape.kind = TypeKind::Object;
        shape.declaration_name = name;
        shape
    }

    fn storage(
        records: Vec<(TypeName, LibraryType)>,
        identity: TypeName,
    ) -> Option<(TypeName, String)> {
        let module = BackendModuleFacts::from_classifiers(records, []).unwrap();
        of(
            &CheckedBackendClassifiers::new(&module, &EmptySymbolSource),
            identity,
        )
    }

    #[test]
    fn a_companion_uses_its_retained_holder_and_authoritative_field() {
        let holder = type_name("storage/Owner$Literal");
        let companion = holder.nested_child("Part$Name");
        let mut owner = LibraryType::declaration_header();
        owner.companion_object = Some(("published$Field".into(), companion));
        let shape = object(Some(ClassifierDeclarationName::Nested {
            owner: holder,
            name: "Part$Name".into(),
        }));
        assert_eq!(
            storage(vec![(holder, owner), (companion, shape)], companion),
            Some((holder, "published$Field".to_string()))
        );
    }

    #[test]
    fn a_declared_nested_object_keeps_its_own_instance_field() {
        let holder = type_name("storage/Owner");
        let identity = holder.nested_child("Ordinary$Object");
        let mut owner = LibraryType::declaration_header();
        owner.companion_object = Some(("Companion".into(), holder.nested_child("Companion")));
        let shape = object(Some(ClassifierDeclarationName::Nested {
            owner: holder,
            name: "Ordinary$Object".into(),
        }));
        assert_eq!(
            storage(vec![(holder, owner), (identity, shape)], identity),
            Some((identity, "INSTANCE".to_string()))
        );
    }

    #[test]
    fn a_top_level_literal_name_does_not_borrow_a_prefix_holders_field() {
        let identity = type_name("storage/Owner$Part$Name");
        let mut prefix = LibraryType::declaration_header();
        prefix.companion_object = Some(("Name".into(), identity));
        let shape = object(Some(ClassifierDeclarationName::TopLevel {
            package: type_name("storage"),
            name: "Owner$Part$Name".into(),
        }));
        assert_eq!(
            storage(
                vec![(type_name("storage/Owner$Part"), prefix), (identity, shape)],
                identity
            ),
            Some((identity, "INSTANCE".to_string()))
        );
    }

    #[test]
    fn unknown_name_or_missing_declared_holder_does_not_guess_storage() {
        let identity = type_name("storage/Owner$Part$Name");
        assert_eq!(storage(vec![(identity, object(None))], identity), None);
        let shape = object(Some(ClassifierDeclarationName::Nested {
            owner: type_name("storage/Owner"),
            name: "Part$Name".into(),
        }));
        assert_eq!(storage(vec![(identity, shape)], identity), None);
    }
}
