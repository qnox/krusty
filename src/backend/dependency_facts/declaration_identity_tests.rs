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

fn sam_value(
    ir: &mut crate::ir::IrFile,
    method: u32,
    alias: u32,
    default: u32,
) -> crate::ir::ExprId {
    use crate::ir::{IrExpr, IrFunction, IrSamMethod, IrSamMethodFrame, IrSamTarget};
    use crate::types::Ty;
    let implementation = ir.add_fun(IrFunction {
        name: "samImplementation".into(),
        params: vec![],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![],
    });
    let frame = |id| IrSamMethodFrame {
        method: Some(IrSamMethod::External(ExternalCallableId::from_raw(id))),
        parameters: vec![],
        result: Ty::Unit,
    };
    ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 0,
        captures: vec![],
        inline_body: None,
        sam: Some(Box::new(IrSamTarget {
            classifier: crate::types::type_name("sample/Action"),
            method: "run".into(),
            method_target: IrSamMethod::External(ExternalCallableId::from_raw(method)),
            parameters: vec![],
            result: Ty::Unit,
            declared_parameters: vec![],
            declared_result: Ty::Unit,
            contravariant_parameters: vec![],
            context_count: 0,
            has_receiver: false,
            suspend: false,
            source_suspend: false,
            requires_object_dispatch: false,
            overridden_results: vec![],
            dispatch_methods: vec![frame(method), frame(alias)],
            default_methods: vec![frame(default)],
            function_adapter: false,
            wraps_function_value: false,
            nullable: false,
            kotlin_interface: true,
            parameter_identities: vec![],
            type_parameters: vec![],
            placement: crate::ir::IrSamPlacement::ThisFile,
        })),
    })
}

#[test]
fn only_emitted_sam_values_freeze_method_headers_and_they_are_not_body_calls() {
    use crate::ir::{IrFile, IrFunction};
    use crate::types::Ty;
    struct Missing;
    impl crate::symbol_source::SymbolSource for Missing {}
    let mut ir = IrFile::default();
    let orphan = sam_value(&mut ir, 7, 11, 19);
    let template = ir.add_fun(IrFunction {
        name: "cloningOnly".into(),
        params: vec![],
        ret: Ty::obj("sample/Action"),
        body: Some(orphan),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![],
    });
    ir.inline_only_fns.insert(template);
    let unused = references::referenced_dependencies(&ir);
    assert!(unused.sam_declarations.is_empty());
    assert!(unused.callables.is_empty());
    let facts = CheckedBackendCallables::freeze(&ir, &Missing).unwrap();
    assert!(facts
        .sam_declaration(ExternalCallableId::from_raw(7))
        .is_none());
    assert!(facts.klib_body_callables().is_empty());

    let live = sam_value(&mut ir, 31, 47, 63);
    ir.add_fun(IrFunction {
        name: "make".into(),
        params: vec![],
        ret: Ty::obj("sample/Action"),
        body: Some(live),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![],
    });
    let emitted = references::referenced_dependencies(&ir);
    assert_eq!(
        emitted.sam_declarations,
        [31, 47, 63]
            .map(ExternalCallableId::from_raw)
            .into_iter()
            .collect()
    );
    assert!(emitted.callables.is_empty());
    assert!(matches!(CheckedBackendCallables::freeze(&ir, &Missing),
        Err(DependencyFactError::UnknownCallable(identity)) if identity == ExternalCallableId::from_raw(31)));
}
