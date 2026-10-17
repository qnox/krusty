use super::*;

#[test]
fn anonymous_owned_edges_select_exact_capture_free_and_capture_owning_substitutions() {
    let mut ir = crate::ir::IrFile::default();
    let body = ir.add_expr(IrExpr::UnitInstance);
    let caller_value = ir.add_expr(IrExpr::GetValue(0));
    let captured = ir.add_expr(IrExpr::Lambda {
        impl_fn: 0,
        arity: 0,
        captures: vec![caller_value],
        sam: None,
        inline_body: Some(body),
    });
    let capture_free = ir.add_expr(IrExpr::Lambda {
        impl_fn: 1,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(body),
    });
    let unrelated = ir.add_expr(IrExpr::Lambda {
        impl_fn: 2,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(body),
    });
    let constructor_lambda = ir.add_expr(IrExpr::Lambda {
        impl_fn: 3,
        arity: 0,
        captures: vec![captured, capture_free, unrelated],
        sam: None,
        inline_body: Some(body),
    });

    let name = crate::types::type_name("sample/InlineAnonymous");
    let mut class = crate::ir::IrClass::synthetic(name);
    class.is_anonymous_object = true;
    class.ctor_args.push(crate::ir::IrCtorArg {
        name: None,
        context_kind: crate::types::ContextParameterKind::None,
        ty: Ty::fun(Vec::new(), Ty::Unit),
        declared_ty: None,
        is_field: true,
        field_index: Some(0),
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: None,
        provenance: crate::ir::IrCtorParameterProvenance::Capture,
        capture_identity: None,
    });
    ir.add_class(class);
    let construction = ir.add_expr(IrExpr::New {
        internal: name,
        args: vec![constructor_lambda],
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });

    let owned_edges = owned_substituted_lambda_capture_edges(
        &ir,
        [construction],
        &HashSet::from_iter([captured, capture_free]),
    );
    assert_eq!(
        owned_edges,
        HashSet::from_iter([
            (constructor_lambda, captured),
            (constructor_lambda, capture_free),
        ])
    );
    assert!(!owned_edges.contains(&(constructor_lambda, unrelated)));
}
