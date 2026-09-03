use super::*;

#[test]
fn build_trivial_function_ir() {
    // Model `fun answer(): Int = 42` in the IR by hand (lowering comes in a later phase).
    let mut f = IrFile::default();
    let lit = f.add_expr(IrExpr::Const(IrConst::Int(42)));
    let ret = f.add_expr(IrExpr::Return(Some(lit)));
    let body = f.add_expr(IrExpr::Block {
        stmts: vec![ret],
        value: None,
    });
    let fun = f.add_fun(IrFunction {
        name: "answer".to_string(),
        params: vec![],
        ret: Ty::obj("kotlin/Int"),
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    assert_eq!(f.functions[fun as usize].name, "answer");
    // The return type is a Kotlin FqName, not a JVM descriptor — the backend maps it.
    match f.functions[fun as usize].ret.obj_internal() {
        Some(fq) => assert_eq!(fq, "kotlin/Int"),
        None => panic!("expected class type"),
    }
    assert!(matches!(f.expr(body), IrExpr::Block { .. }));
}

#[test]
fn expr_diverges_by_handles_branches_and_custom_leaves() {
    let mut f = IrFile::default();
    let condition = f.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let first = f.add_expr(IrExpr::Return(None));
    let second = f.add_expr(IrExpr::Throw { operand: condition });
    let exhaustive = f.add_expr(IrExpr::When {
        branches: vec![(Some(condition), first), (None, second)],
    });
    let non_exhaustive = f.add_expr(IrExpr::When {
        branches: vec![(Some(condition), first)],
    });
    let custom_leaf = f.add_expr(IrExpr::Const(IrConst::Int(1)));
    let unreachable_value = f.add_expr(IrExpr::Const(IrConst::Int(2)));
    let divergent_value_block = f.add_expr(IrExpr::Block {
        stmts: vec![second],
        value: Some(unreachable_value),
    });
    let coerced_divergent_block = f.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: divergent_value_block,
        type_operand: Ty::Nothing,
    });

    assert!(f.expr_diverges_by(exhaustive, &|_, _| false));
    assert!(!f.expr_diverges_by(non_exhaustive, &|_, _| false));
    assert!(f.expr_diverges_by(custom_leaf, &|id, _| id == custom_leaf));
    assert!(f.expr_diverges_by(divergent_value_block, &|_, _| false));
    assert!(f.expr_diverges_by(coerced_divergent_block, &|_, _| false));
}

#[test]
fn zero_for_value_type_tracks_primitive_carriers() {
    assert_eq!(
        IrConst::zero_for_value_type(Ty::Boolean),
        IrConst::Boolean(false)
    );
    assert_eq!(IrConst::zero_for_value_type(Ty::Int), IrConst::Int(0));
    assert_eq!(IrConst::zero_for_value_type(Ty::UInt), IrConst::Int(0));
    assert_eq!(IrConst::zero_for_value_type(Ty::ULong), IrConst::Long(0));
    assert_eq!(IrConst::zero_for_value_type(Ty::String), IrConst::Null);
}

#[test]
fn shift_value_indices_shifts_lambda_captures_not_inline_body() {
    // A `Lambda` whose CAPTURE references the enclosing slot 1 and whose `inline_body` references the
    // lambda's OWN slot 1. Shifting the enclosing scope (threshold 1, +2) must shift the capture
    // (1 → 3) but leave the lambda-internal `inline_body` reference (1) untouched.
    let mut f = IrFile::default();
    let cap = f.add_expr(IrExpr::GetValue(1)); // capture of enclosing value 1
    let inner = f.add_expr(IrExpr::GetValue(1)); // the lambda's OWN value 1
    let lam = f.add_expr(IrExpr::Lambda {
        impl_fn: 0,
        arity: 0,
        captures: vec![cap],
        sam: None,
        inline_body: Some(inner),
    });
    let outer = f.add_expr(IrExpr::GetValue(1)); // an enclosing value 1, sibling of the lambda
    let block = f.add_expr(IrExpr::Block {
        stmts: vec![lam],
        value: Some(outer),
    });
    shift_value_indices(&mut f, block, 1, 2);
    assert!(
        matches!(f.exprs[cap as usize], IrExpr::GetValue(3)),
        "capture must shift 1 -> 3"
    );
    assert!(
        matches!(f.exprs[outer as usize], IrExpr::GetValue(3)),
        "enclosing ref must shift 1 -> 3"
    );
    assert!(
        matches!(f.exprs[inner as usize], IrExpr::GetValue(1)),
        "lambda-internal inline_body ref must NOT shift"
    );
}

#[test]
fn shift_value_indices_rewrites_shared_children_once() {
    let mut f = IrFile::default();
    let shared = f.add_expr(IrExpr::GetValue(1));
    let block = f.add_expr(IrExpr::Block {
        stmts: vec![shared],
        value: Some(shared),
    });

    shift_value_indices(&mut f, block, 1, 1);

    assert!(matches!(f.expr(shared), IrExpr::GetValue(2)));
}

#[test]
fn ir_field_new_uses_kotlin_defaults() {
    let f = IrField::new("x".to_string(), Ty::Int);
    assert_eq!(f.name, "x");
    assert_eq!(f.ty, Ty::Int);
    assert_eq!(f.type_param, None);
    assert_eq!(f.default, None);
    // Kotlin default: private backing field, not known-final, not lateinit.
    assert!(f.is_private());
    assert!(!f.is_final());
    assert!(!f.is_lateinit());
}

#[test]
fn arena_builders_append_and_index() {
    let mut f = IrFile::default();
    let a = f.add_expr(IrExpr::Const(IrConst::Int(1)));
    let b = f.add_expr(IrExpr::Const(IrConst::Int(2)));
    assert_eq!(a, 0);
    assert_eq!(b, 1);
    assert!(matches!(f.expr(a), IrExpr::Const(IrConst::Int(1))));

    let fid = f.add_fun(IrFunction {
        name: "g".to_string(),
        params: vec![],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    assert_eq!(fid, 0);
    let cid = f.add_class(IrClass {
        fq_name: "demo/C".into(),
        ..blank_class("demo/C")
    });
    assert_eq!(cid, 0);
    assert!(f.classes[cid as usize].fq_name_matches("demo/C"));
}

#[test]
fn for_each_child_visits_every_direct_operand() {
    let mut f = IrFile::default();
    let lhs = f.add_expr(IrExpr::Const(IrConst::Int(1)));
    let rhs = f.add_expr(IrExpr::Const(IrConst::Int(2)));
    let bin = f.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Add,
        lhs,
        rhs,
    });
    let mut kids = Vec::new();
    for_each_child(&f.exprs, bin, &mut |c| kids.push(c));
    assert_eq!(kids, vec![lhs, rhs]);

    // A leaf node (Const) has no children.
    let mut none = Vec::new();
    for_each_child(&f.exprs, lhs, &mut |c| none.push(c));
    assert!(none.is_empty());

    // A block visits its statements then its value.
    let blk = f.add_expr(IrExpr::Block {
        stmts: vec![lhs, rhs],
        value: Some(bin),
    });
    let mut bk = Vec::new();
    for_each_child(&f.exprs, blk, &mut |c| bk.push(c));
    assert_eq!(bk, vec![lhs, rhs, bin]);
}

/// A minimal well-formed `IrClass` for tests that only exercise fields/functions on the file.
fn blank_class(fq: &str) -> IrClass {
    IrClass {
        fq_name: fq.into(),
        is_source_declared: false,
        is_anonymous_object: false,
        enclosing_function: None,
        is_inner_class: false,
        is_local_class: false,
        is_value: false,
        is_data: false,
        decl_line: 0,
        type_param_bounds: Vec::new(),
        type_params: Vec::new(),
        captured_type_params: Vec::new(),
        supertypes: Vec::new(),
        properties: Vec::new(),
        fields: Vec::new(),
        field_annotations: Vec::new(),
        property_annotations: Vec::new(),
        ctor_param_count: 0,
        constructor_prefix_count: 0,
        ctor_args: Vec::new(),
        ctor_param_annotations: Vec::new(),
        init_body: None,
        pre_super_param_fields: Vec::new(),
        explicit_param_stores: false,
        methods: Vec::new(),
        is_interface: false,
        is_fun_interface: false,
        is_annotation: false,
        annotation_impl_of: None,
        is_sealed: false,
        sealed_subclasses: Default::default(),
        is_abstract: false,
        is_open: false,
        superclass: "kotlin/Any".into(),
        super_args: Vec::new(),
        super_ctor_params: Vec::new(),
        enum_entries: Vec::new(),
        enum_entry_of: None,
        prop_ref: None,
        func_ref: None,
        bridges: Vec::new(),
        interfaces: Default::default(),
        is_object: false,
        is_companion: false,
        companion_class: None,
        secondary_ctors: Vec::new(),
        has_primary_ctor: true,
        applied_annotations: DeclarationAnnotations::default(),
        primary_ctor_annotations: DeclarationAnnotations::default(),
        annotation_retention: None,
    }
}

fn add_toplevel_fn(f: &mut IrFile, name: &str, param: Ty) -> u32 {
    f.add_fun(IrFunction {
        name: name.to_string(),
        params: vec![param],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    })
}

#[test]
fn toplevel_default_stub_safe_accepts_a_simple_constant_default() {
    let mut f = IrFile::default();
    let fid = add_toplevel_fn(&mut f, "greet", Ty::Int);
    let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
    f.fn_params
        .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
    assert!(toplevel_default_stub_safe(&f, fid));
}

#[test]
fn toplevel_default_stub_safe_accepts_mangled_suspend_and_rejects_missing_defaults() {
    // A value-class-MANGLED name (the post-pass view of a VC-param function) emits the mangled
    // `foo-<hash>$default` stub, kotlinc's shape — accepted since the erased params carry no
    // extra carve-out evidence.
    let mut f = IrFile::default();
    let fid = add_toplevel_fn(&mut f, "greet-abc123", Ty::Int);
    let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
    f.fn_params
        .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
    assert!(toplevel_default_stub_safe(&f, fid));

    // A mangled SUSPEND function stays accepted: the CPS-appended Continuation is just another
    // loaded stub parameter (kotlinc's pick-<hash>$default shape), and the constant-only
    // default restriction already excludes anything that could suspend.
    f.suspend_funs.push(fid);
    assert!(toplevel_default_stub_safe(&f, fid));

    let mut g = IrFile::default();
    let gid = add_toplevel_fn(&mut g, "hello", Ty::Int);
    assert!(!toplevel_default_stub_safe(&g, gid));
}

#[test]
fn toplevel_default_stub_safe_allows_overloaded_and_rejects_unsafe_default() {
    let mut f = IrFile::default();
    let fid = add_toplevel_fn(&mut f, "over", Ty::Int);
    add_toplevel_fn(&mut f, "over", Ty::String);
    let def = f.add_expr(IrExpr::Const(IrConst::Int(0)));
    f.fn_params
        .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
    assert!(toplevel_default_stub_safe(&f, fid));

    let mut g = IrFile::default();
    let gid = add_toplevel_fn(&mut g, "spill", Ty::Int);
    let bad = g.add_expr(IrExpr::GetValue(3));
    g.fn_params
        .insert(gid, FnParamInfo::defaults(Vec::new(), vec![Some(bad)]));
    assert!(!toplevel_default_stub_safe(&g, gid));
}

#[test]
fn toplevel_default_stub_safe_accepts_a_lambda_capturing_only_parameters() {
    // `fun foo(base: Int, f: () -> Int = { base })` — the default lambda captures a PARAMETER
    // (value 0, in scope inside the stub), so the stub can re-emit the closure construction.
    let mut f = IrFile::default();
    let fid = f.add_fun(IrFunction {
        name: "foo".to_string(),
        params: vec![Ty::Int, Ty::obj("kotlin/jvm/functions/Function0")],
        ret: Ty::Int,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let cap = f.add_expr(IrExpr::GetValue(0));
    let lam = f.add_expr(IrExpr::Lambda {
        impl_fn: 0,
        arity: 0,
        captures: vec![cap],
        sam: None,
        inline_body: None,
    });
    f.fn_params.insert(
        fid,
        FnParamInfo::defaults(Vec::new(), vec![None, Some(lam)]),
    );
    assert!(toplevel_default_stub_safe(&f, fid));
}

#[test]
fn toplevel_default_stub_safe_rejects_a_lambda_capturing_a_spilled_temp() {
    // A capture beyond the parameter range (a spilled temp / enclosing local) is not in scope in
    // the static stub frame — rejected.
    let mut f = IrFile::default();
    let fid = add_toplevel_fn(&mut f, "foo", Ty::Int);
    let cap = f.add_expr(IrExpr::GetValue(7));
    let lam = f.add_expr(IrExpr::Lambda {
        impl_fn: 0,
        arity: 0,
        captures: vec![cap],
        sam: None,
        inline_body: None,
    });
    f.fn_params
        .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(lam)]));
    assert!(!toplevel_default_stub_safe(&f, fid));
}

#[test]
fn toplevel_default_stub_safe_accepts_locals_declared_inside_the_default() {
    let mut f = IrFile::default();
    let fid = add_toplevel_fn(&mut f, "foo", Ty::obj("kotlin/String"));
    let init = f.add_expr(IrExpr::Const(IrConst::String("x".to_string().into())));
    let declaration = f.add_expr(IrExpr::Variable {
        index: 1,
        ty: Ty::obj("kotlin/String"),
        init: Some(init),
        named: false,
    });
    let value = f.add_expr(IrExpr::GetValue(1));
    let block = f.add_expr(IrExpr::Block {
        stmts: vec![declaration],
        value: Some(value),
    });
    f.fn_params
        .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(block)]));

    assert!(toplevel_default_stub_safe(&f, fid));
}

#[test]
fn toplevel_default_stub_safe_value_class_param_pre_mangling() {
    // The PRE-pass view of a VC-param function: plain name, value-class-typed param. A plain
    // non-nullable underlying routes through the (soon-mangled) stub; a NULLABLE underlying stays
    // boxed in kotlinc's stub signature (not modeled) — rejected.
    let mut f = IrFile::default();
    let mut c = blank_class("X");
    c.is_value = true;
    c.fields.push(IrField {
        name: "s".to_string(),
        ty: Ty::String,
        type_param: None,
        default: None,
        flags: IrfFlags::default().with_is_final(true),
    });
    f.add_class(c);
    let fid = add_toplevel_fn(&mut f, "foo", Ty::obj("X"));
    let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
    f.fn_params
        .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
    assert!(toplevel_default_stub_safe(&f, fid));

    f.classes[0].fields[0].ty = Ty::nullable(Ty::String);
    assert!(!toplevel_default_stub_safe(&f, fid));
}

#[test]
fn default_stub_trusts_value_construction_provenance_not_helper_spelling() {
    // A generated-looking JVM name is not proof that a call came from value-class lowering: source
    // and dependency declarations can use escaped identifiers. Keep the call rejected until the
    // value-class pass explicitly records that this expression replaced a semantic construction.
    let mut f = IrFile::default();
    let fid = add_toplevel_fn(&mut f, "consume", Ty::Int);
    let call = f.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: crate::types::type_name("example/Wrapper"),
            name: "constructor-impl".to_string(),
            descriptor: "(I)I".to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: Vec::new(),
    });
    f.fn_params
        .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(call)]));

    assert!(!toplevel_default_stub_safe(&f, fid));

    f.record_erased_value_construction(call, crate::types::type_name("example/Wrapper"), Ty::Int);
    assert!(toplevel_default_stub_safe(&f, fid));
}

#[test]
fn default_stub_uses_external_value_class_metadata_without_a_source_class() {
    // Dependency metadata and same-file declarations must enter the representation checks through
    // one query. No synthetic `IrClass` is added here: the external semantic record alone identifies
    // the value class and its primitive underlying slot.
    let mut f = IrFile::default();
    let wrapper = crate::types::type_name("dependency/Wrapper");
    f.insert_external_value_class_name(wrapper, Ty::Int);
    let fid = add_toplevel_fn(&mut f, "consume", Ty::obj_name(wrapper));
    let argument = f.add_expr(IrExpr::Const(IrConst::Int(7)));
    let construction = f.add_expr(IrExpr::New {
        internal: wrapper,
        args: vec![argument],
        ctor_params: Some(vec![Ty::Int]),
        ctor_desc: None,
        external_target: None,
    });
    f.fn_params.insert(
        fid,
        FnParamInfo::defaults(Vec::new(), vec![Some(construction)]),
    );

    assert!(toplevel_default_stub_safe(&f, fid));
}
