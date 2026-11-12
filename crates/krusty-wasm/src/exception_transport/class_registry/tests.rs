use super::*;
use crate::{
    exception_transport::ExceptionTransport, identity::IdentityObjects, objects::ObjectRoot,
    runtime::Runtime,
};
use krusty::backend::{BackendModuleFacts, CheckedBackendClassifiers};
use krusty::libraries::LibraryType;

fn runtime() -> (Module, ExceptionTransport) {
    let mut module = Module::default();
    let (_, string) = Runtime::declare_types(&mut module);
    let objects = ObjectRoot::declare(&mut module, ValType::nullable(string));
    let identity = IdentityObjects::declare(&mut module, &objects);
    let transport =
        ExceptionTransport::declare(&mut module, &objects, &identity, ValType::nullable(string));
    (module, transport)
}

fn declarations() -> Vec<(TypeName, LibraryType)> {
    use RuntimeExceptionKind::*;
    [
        (Throwable, None),
        (Exception, Some(Throwable)),
        (RuntimeException, Some(Exception)),
        (IndexOutOfBounds, Some(RuntimeException)),
    ]
    .into_iter()
    .map(|(kind, parent)| {
        let mut shape = LibraryType::declaration_header();
        shape.runtime_exception = Some(RuntimeExceptionLayout {
            kind,
            parent: parent.map(|kind| kind.classifier()),
        });
        if let Some(parent) = parent {
            shape.supertypes.push_name(parent.classifier());
        }
        (kind.classifier(), shape)
    })
    .collect()
}

#[test]
fn a_cached_leaf_still_authenticates_every_ancestor_without_partial_publication() {
    let (mut module, mut transport) = runtime();
    let identity = RuntimeExceptionKind::IndexOutOfBounds.classifier();
    let parent = RuntimeExceptionKind::RuntimeException.classifier();
    let facts = BackendModuleFacts::from_classifiers(declarations(), []).unwrap();
    let source = CheckedBackendClassifiers::new(&facts, &krusty::libraries::EmptySymbolSource);
    let structure = transport
        .ensure_class(&mut module, &source, identity)
        .unwrap();
    let before = module.encode();
    let cached = transport.classes.declared.clone();
    assert_eq!(
        transport.ensure_class(&mut module, &source, identity),
        Ok(structure)
    );
    assert_eq!(module.encode(), before);
    for (records, expected) in [
        (
            declarations()
                .into_iter()
                .filter(|(name, _)| *name != parent)
                .collect::<Vec<_>>(),
            format!("no frozen runtime exception declaration for {parent}"),
        ),
        (
            declarations()
                .into_iter()
                .map(|(name, mut shape)| {
                    if name == parent {
                        shape.runtime_exception = None;
                    }
                    (name, shape)
                })
                .collect(),
            format!("no authenticated runtime exception layout for {parent}"),
        ),
        (
            declarations()
                .into_iter()
                .map(|(name, mut shape)| {
                    if name == parent {
                        shape.runtime_exception.as_mut().unwrap().parent = Some(identity);
                    }
                    (name, shape)
                })
                .collect(),
            format!("runtime exception declaration {parent} changed during emission"),
        ),
    ] {
        let facts = BackendModuleFacts::from_classifiers(records, []).unwrap();
        let source = CheckedBackendClassifiers::new(&facts, &krusty::libraries::EmptySymbolSource);
        assert_eq!(
            transport.ensure_class(&mut module, &source, identity),
            Err(expected)
        );
        assert_eq!(transport.classes.declared, cached);
        assert_eq!(module.encode(), before);
    }
}

#[test]
fn a_cyclic_exception_parent_declines_before_declaring_any_class() {
    let (mut module, mut transport) = runtime();
    let identity = RuntimeExceptionKind::IndexOutOfBounds.classifier();
    let mut records = declarations();
    records
        .iter_mut()
        .find(|(name, _)| *name == RuntimeExceptionKind::RuntimeException.classifier())
        .unwrap()
        .1
        .runtime_exception
        .as_mut()
        .unwrap()
        .parent = Some(identity);
    let facts = BackendModuleFacts::from_classifiers(records, []).unwrap();
    let source = CheckedBackendClassifiers::new(&facts, &krusty::libraries::EmptySymbolSource);
    let before = module.encode();
    assert_eq!(
        transport.ensure_class(&mut module, &source, identity),
        Err(format!(
            "a cyclic runtime exception superclass at {identity}"
        ))
    );
    assert!(transport.classes.declared.is_empty());
    assert_eq!(module.encode(), before);
}

#[test]
fn selected_wasm_stdlibs_publish_the_exception_chain_without_constructor_authority() {
    use krusty::compilation_target::CompilationTarget;
    use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
    for target in [CompilationTarget::WasmJs, CompilationTarget::WasmWasi] {
        let path = krusty::toolchain::kotlin_stdlib_klib(target)
            .expect("the exact target stdlib must be provisioned");
        let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
            path,
            KlibStdlibSemantics::Wasm,
        )])
        .unwrap();
        let module_facts = BackendModuleFacts::from_classifiers([], []).unwrap();
        let source = CheckedBackendClassifiers::new(&module_facts, &libraries);
        let (mut module, mut transport) = runtime();
        transport
            .ensure_class(
                &mut module,
                &source,
                RuntimeExceptionKind::IndexOutOfBounds.classifier(),
            )
            .unwrap();
        assert_eq!(transport.classes.declared.len(), 4);
        assert_eq!(
            transport.classes.declared[&RuntimeExceptionKind::IndexOutOfBounds.classifier()]
                .0
                .parent,
            Some(RuntimeExceptionKind::RuntimeException.classifier())
        );
    }
}

#[test]
fn selected_illegal_state_storage_is_a_distinct_subtype_of_its_declared_parent() {
    use krusty::compilation_target::CompilationTarget;
    use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
    use krusty::symbol_source::SymbolSource;
    for target in [CompilationTarget::WasmJs, CompilationTarget::WasmWasi] {
        let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
            krusty::toolchain::kotlin_stdlib_klib(target)
                .expect("the exact target stdlib must be provisioned"),
            KlibStdlibSemantics::Wasm,
        )])
        .unwrap();
        let facts = BackendModuleFacts::from_classifiers([], []).unwrap();
        let source = CheckedBackendClassifiers::new(&facts, &libraries);
        let (mut module, mut transport) = runtime();
        let owner = krusty::types::type_name("kotlin/IllegalStateException");
        let declaration = libraries
            .classifier(owner)
            .expect("the selected class exists");
        for (params, message) in [
            (Vec::new(), false),
            (
                vec![krusty::types::Ty::nullable(krusty::types::Ty::String)],
                true,
            ),
        ] {
            let selected = declaration
                .constructors
                .iter()
                .filter(|constructor| constructor.params == params)
                .collect::<Vec<_>>();
            let [constructor] = selected.as_slice() else {
                panic!("one exact selected zero-argument or nullable-message constructor")
            };
            let operation = krusty::libraries::CompilerIntrinsic::ThrowableConstructor {
                kind: RuntimeExceptionKind::IllegalState,
                message,
            };
            assert_eq!(constructor.owner, Some(owner));
            assert_eq!(
                constructor.realization,
                krusty::libraries::MemberRealization::Intrinsic(operation)
            );
            let callable = libraries
                .external_callable(constructor.external_identity.unwrap())
                .unwrap();
            assert_eq!(
                callable.kind,
                krusty::libraries::ExternalCallableKind::Constructor
            );
            assert_eq!(
                callable.declaration_owner,
                Some(krusty::types::SemanticCallableOwner::Classifier(owner))
            );
            assert_eq!(callable.callable.params, params);
            assert_eq!(
                callable.callable.member_realization,
                krusty::libraries::MemberRealization::Intrinsic(operation)
            );
        }
        let selected = transport.ensure_class(&mut module, &source, owner).unwrap();
        let parent = transport
            .ensure_class(
                &mut module,
                &source,
                RuntimeExceptionKind::RuntimeException.classifier(),
            )
            .unwrap();
        assert_ne!(selected, parent);
        assert_ne!(selected, transport.structure);
        assert_eq!(transport.classes.declared[&owner].1, selected);
        assert_eq!(
            transport.classes.declared[&owner].0.parent,
            Some(RuntimeExceptionKind::RuntimeException.classifier())
        );
    }
}
