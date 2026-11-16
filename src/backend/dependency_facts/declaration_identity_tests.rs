//! Declaration evidence must not become an executable dependency use.

use super::*;

#[test]
fn inherited_member_identity_does_not_root_its_body() {
    let ir = crate::ir::IrFile::default();
    let references_before = references::referenced_dependencies(&ir);
    let identity = ExternalCallableId::from_raw(7);
    let (_, declaration) = crate::libraries::klib_body_class_fixture::generic_member_declaration();
    let mut facts = CheckedBackendCallables::default();
    facts
        .freeze_declaration_identities([identity], |_| Ok(Some(declaration.clone())))
        .unwrap();
    assert_eq!(facts.declaration_identity(identity), Some(&declaration));
    assert!(facts.callable(identity).is_none());
    assert!(facts.klib_body_callables().is_empty());
    assert_eq!(references::referenced_dependencies(&ir), references_before);
}

#[test]
fn unknown_header_identity_preserves_the_exact_freeze_error() {
    let identity = ExternalCallableId::from_raw(11);
    let mut facts = CheckedBackendCallables::default();
    let result = facts.freeze_declaration_identities([identity], |identity| {
        Err(DependencyFactError::UnknownCallable(identity))
    });
    assert_eq!(result, Err(DependencyFactError::UnknownCallable(identity)));
    assert!(facts.declaration_identity(identity).is_none());
    assert!(facts.klib_body_callables().is_empty());
}
