use super::*;
use krusty::backend::BackendModuleFacts;
use krusty::libraries::{LibraryType, TypeKind};
use krusty::types::{type_name, Ty};

/// The frozen facts of a module declaring only `Shared`, of `kind`.
fn facts(kind: TypeKind) -> BackendModuleFacts {
    let mut shape = LibraryType::declaration_header();
    shape.kind = kind;
    BackendModuleFacts::from_classifiers([(type_name("Shared"), shape)], []).unwrap()
}

fn reader(classifier: krusty::types::TypeName) -> IrFile {
    let mut ir = IrFile::default();
    let value = ir.add_expr(IrExpr::SingletonValue { classifier });
    ir.set_logical_type(value, Ty::obj_name(classifier));
    ir.add_fun(krusty::ir::IrFunction {
        name: "read".to_owned(),
        params: vec![],
        ret: Ty::obj_name(classifier),
        body: Some(value),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![],
    });
    ir
}

fn defining_file() -> IrFile {
    let mut ir = IrFile::default();
    let mut class = krusty::ir::test_support::blank_class("Shared");
    class.is_object = true;
    class.is_source_declared = true;
    class.has_primary_ctor = true;
    ir.add_class(class);
    ir
}

#[test]
fn a_forward_source_read_reserves_the_defining_files_real_getter() {
    let classifier = type_name("Shared");
    let facts = facts(TypeKind::Object);
    let mut registry = CallableRegistry::default();
    // The defining file's classes are laid out over a started module's runtime.
    let mut started = super::super::Started::new(crate::target::WasmTarget::Js);
    let wasm = &mut started.module;
    registry.enter_source_module("main").unwrap();
    let caller = registry
        .file_calls(
            &reader(classifier),
            &CheckedBackendCallables::default(),
            wasm,
            Some(("main", &facts)),
        )
        .unwrap();
    let reserved = caller.singleton(classifier).expect("forward getter");
    assert!(registry.check_source_singletons().is_err());
    let ir = defining_file();
    let definition = registry
        .file_calls(
            &ir,
            &CheckedBackendCallables::default(),
            wasm,
            Some(("main", &facts)),
        )
        .unwrap();
    assert_eq!(definition.singleton_definition(0), Some(reserved));

    let classifiers = krusty::backend::CheckedBackendClassifiers::new(
        &facts,
        &krusty::libraries::EmptySymbolSource,
    );
    let classes = super::super::classes::declare(
        &ir,
        &mut started.module,
        &mut started.runtime,
        super::super::classes::ClassEntryPoints {
            functions: &[],
            singletons: &definition,
        },
        &[],
        super::super::classes::BeyondFile {
            classifiers: &classifiers,
            plan: &krusty::backend::class_tables::module_slots::ModuleSlotPlan::empty(),
            laid_out: &super::super::laid_out_classes::LaidOutClasses::default(),
        },
    )
    .unwrap();
    assert_eq!(
        classes.forms[0].singleton.as_ref().unwrap().getter,
        reserved
    );
    assert_eq!(registry.source_singletons[&classifier].function, reserved);
    assert!(!registry.source_singletons[&classifier].defined);
}

#[test]
fn source_getters_do_not_cross_module_or_dependency_support_boundaries() {
    let classifier = type_name("Shared");
    let facts = facts(TypeKind::Object);
    let mut registry = CallableRegistry::default();
    let mut wasm = Module::default();
    registry.enter_source_module("lib").unwrap();
    let definitions = registry
        .file_calls(
            &defining_file(),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("lib", &facts)),
        )
        .unwrap();
    let first = definitions.singleton_definition(0).unwrap();
    registry.define_source_singletons(&definitions).unwrap();
    assert_eq!(registry.check_source_singletons(), Ok(()));
    let support = registry
        .file_calls(
            &reader(classifier),
            &CheckedBackendCallables::default(),
            &mut wasm,
            None,
        )
        .unwrap();
    assert_eq!(support.singleton(classifier), None);
    registry.enter_source_module("app").unwrap();
    let second = registry
        .file_calls(
            &reader(classifier),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("app", &facts)),
        )
        .unwrap();
    assert_ne!(second.singleton(classifier), Some(first));
    assert!(registry
        .file_calls(
            &reader(classifier),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("lib", &facts))
        )
        .is_err());
}

#[test]
fn unresolved_and_duplicate_source_definitions_fail_without_losing_the_epoch() {
    let facts = facts(TypeKind::Object);
    let mut registry = CallableRegistry::default();
    let mut wasm = Module::default();
    registry.enter_source_module("main").unwrap();
    registry
        .file_calls(
            &reader(type_name("Shared")),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("main", &facts)),
        )
        .unwrap();
    let expected = Err("a source-module object without its defining singleton getter".to_owned());
    assert_eq!(registry.check_source_singletons(), expected);
    assert_eq!(registry.enter_source_module("other"), expected);
    assert_eq!(registry.source_module.as_deref(), Some("main"));
    let definitions = registry
        .file_calls(
            &defining_file(),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("main", &facts)),
        )
        .unwrap();
    assert!(registry
        .file_calls(
            &defining_file(),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("main", &facts))
        )
        .is_err());
    registry.define_source_singletons(&definitions).unwrap();
    assert!(registry.define_source_singletons(&definitions).is_err());
    assert_eq!(registry.check_source_singletons(), Ok(()));
}

#[test]
fn a_defining_object_requires_the_frozen_source_object_role() {
    let facts = facts(TypeKind::Class);
    let mut registry = CallableRegistry::default();
    let mut wasm = Module::default();
    registry.enter_source_module("main").unwrap();
    assert!(registry
        .file_calls(
            &defining_file(),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("main", &facts))
        )
        .is_err());
    assert!(registry.source_singletons.is_empty());
    // An equal dependency classifier cannot authenticate an invalid source-object read.
    registry
        .dependency_singletons
        .insert(type_name("Shared"), wasm.declare());
    let error = registry
        .file_calls(
            &reader(type_name("Shared")),
            &CheckedBackendCallables::default(),
            &mut wasm,
            Some(("main", &facts)),
        )
        .err()
        .expect("a source class cannot borrow a dependency object's getter");
    assert_eq!(error, "a singleton value of a non-object source classifier");
}
