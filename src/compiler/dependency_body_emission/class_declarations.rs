//! Declaration-only identities of classes physically materialized by a closed support unit.

use crate::backend::DependencyExportSignatures;
use crate::klib_lowering::DependencyBodyUnit;
use crate::libraries::{
    ExternalCallableKind, KlibAccessorRole, KlibClassIdentity, KlibDeclarationSignature,
    SemanticPlatform, TypeKind,
};
use crate::types::SemanticCallableOwner;

pub(super) fn record(
    unit: &DependencyBodyUnit,
    platform: &dyn SemanticPlatform,
    exports: &mut DependencyExportSignatures,
) -> Result<(), &'static str> {
    let ir = unit.ir();
    let mut classes = crate::fx_hash::HashSet::default();
    let mut identities = crate::fx_hash::HashMap::default();
    for (classifier, constructor) in unit.materialized_classes() {
        let class = ir
            .class_id_by_name(classifier)
            .ok_or("a materialized dependency class lost its actual IR declaration")?;
        let physical = &ir.classes[class as usize];
        // Enum/value layouts have dedicated materialization contracts. This inventory publishes
        // ordinary class definitions only; it does not turn their special entries into ctors.
        if physical.is_enum
            || physical.is_value
            || matches!(
                constructor.declaration,
                KlibDeclarationSignature::FileLocal { .. }
            )
        {
            continue;
        }
        let (_, selected) = platform
            .external_callable_by_dependency(constructor)
            .ok_or("a materialized class lost its authenticated constructor declaration")?;
        let contract = selected
            .klib_body_class_contract
            .as_ref()
            .ok_or("a materialized class has no authenticated declaring-class contract")?;
        if selected.kind != ExternalCallableKind::Constructor
            || selected.dependency_callable.as_ref() != Some(constructor)
            || selected.declaration_owner != Some(SemanticCallableOwner::Classifier(classifier))
            || !contract.matches_owner(&constructor.module, classifier)
            || contract.kind != TypeKind::Class
            || contract.is_value
            || contract.signature.member_id().is_some()
        {
            return Err("a materialized class changed its exact constructor or classifier role");
        }
        let identity = KlibClassIdentity {
            module: contract.module.clone(),
            signature: contract.signature.clone(),
        };
        record_class_identity(class, identity, &mut identities, exports)?;
        classes.insert(classifier);
    }
    for (identity, function) in unit.callable_functions() {
        let declaration = &ir.functions[function as usize];
        let Some(owner) = declaration.dispatch_receiver else {
            continue;
        };
        if declaration.is_static || !classes.contains(&owner) {
            continue;
        }
        let class = ir
            .class_id_by_name(owner)
            .ok_or("a support member lost its physical declaring class")?;
        if !ir.classes[class as usize].methods.contains(&function) {
            continue;
        }
        let (_, selected) = platform
            .external_callable_by_dependency(identity)
            .ok_or("a support member lost its exact provider declaration")?;
        if selected.kind == ExternalCallableKind::Constructor {
            // Initializing helpers may appear in the physical class's method list. Their
            // constructor provenance is authenticated above, independently of virtual members.
            continue;
        }
        if selected.kind != ExternalCallableKind::Member
            || selected.dependency_callable.as_ref() != Some(identity)
            || selected.declaration_owner != Some(SemanticCallableOwner::Classifier(owner))
        {
            return Err("a support member changed its declaring class or declaration role");
        }
        match (&identity.declaration, &selected.klib_body_accessor) {
            (KlibDeclarationSignature::Accessor(_), Some(accessor)) => {
                exports.record_accessor_member_declaration(
                    function,
                    identity.clone(),
                    accessor.role == KlibAccessorRole::Setter,
                );
            }
            (KlibDeclarationSignature::Accessor(_), None) | (_, Some(_)) => {
                return Err("a support accessor lost its authenticated property association");
            }
            (_, None) => exports.record_member_declaration(function, identity.clone()),
        }
    }
    Ok(())
}

fn record_class_identity(
    class: crate::ir::ClassId,
    identity: KlibClassIdentity,
    identities: &mut crate::fx_hash::HashMap<crate::ir::ClassId, KlibClassIdentity>,
    exports: &mut DependencyExportSignatures,
) -> Result<(), &'static str> {
    match identities.get(&class) {
        Some(previous) if previous != &identity => {
            Err("constructors of one materialized class disagree on its declaration identity")
        }
        Some(_) => Ok(()),
        None => {
            exports.record_classifier(class, identity.clone());
            identities.insert(class, identity);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(module: &str) -> KlibClassIdentity {
        KlibClassIdentity {
            module: module.into(),
            signature: crate::metadata::id_signature::KlibPublicIdSignature::classifier(
                crate::types::type_name("sample/Owner"),
            ),
        }
    }

    #[test]
    fn several_authenticated_constructors_publish_one_class_declaration() {
        let mut exports = DependencyExportSignatures::default();
        let mut identities = crate::fx_hash::HashMap::default();
        let selected = identity("producer");
        record_class_identity(3, selected.clone(), &mut identities, &mut exports).unwrap();
        record_class_identity(3, selected.clone(), &mut identities, &mut exports).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(exports.classifier(3), Some(&selected));
        assert!(exports.callables().next().is_none());
    }

    #[test]
    fn changed_constructor_producer_cannot_replace_a_class_declaration() {
        for (first, second) in [("first", "second"), ("second", "first")] {
            let mut exports = DependencyExportSignatures::default();
            let mut identities = crate::fx_hash::HashMap::default();
            let original = identity(first);
            record_class_identity(3, original.clone(), &mut identities, &mut exports).unwrap();
            assert_eq!(
                record_class_identity(3, identity(second), &mut identities, &mut exports),
                Err("constructors of one materialized class disagree on its declaration identity")
            );
            assert_eq!(exports.classifier(3), Some(&original));
        }
    }
}
