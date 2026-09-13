use super::*;

#[test]
fn identity_evidence_is_published_without_changing_ordinary_unification() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let t = Ty::ty_param("owner:T", any);
    let function = Ty::fun(vec![t], t);

    let mut ordinary = GSigBinds::new();
    unify_ty(function, function, &mut ordinary);
    assert!(ordinary.is_empty());

    let mut identity = GSigBinds::new();
    collect_generic_identity_bindings(function, function, &["owner:T".to_string()], &mut identity);
    assert_eq!(identity.get("owner:T"), Some(&t));
}

#[test]
fn identity_evidence_does_not_bind_a_different_declaration_variable() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let declared = Ty::ty_param("callee:T", any);
    let actual = Ty::ty_param("caller:T", any);
    let mut identity = GSigBinds::new();

    collect_generic_identity_bindings(declared, actual, &["callee:T".to_string()], &mut identity);

    assert!(identity.is_empty());
}

#[test]
fn provisional_error_result_does_not_bind_a_generic_formal() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let t = Ty::ty_param("T", any);
    let r = Ty::ty_param("R", any);
    let declared = Ty::fun(vec![t], r);
    let provisional = Ty::fun(vec![Ty::String], Ty::Error);

    let mut ordinary = GSigBinds::new();
    unify_ty(declared, provisional, &mut ordinary);
    assert_eq!(ordinary.get("T"), Some(&Ty::String));
    assert!(!ordinary.contains_key("R"));

    let mut inferred = GSigBinds::new();
    unify_inferred_ty(declared, provisional, &mut inferred);
    assert_eq!(inferred.get("T"), Some(&Ty::String));
    assert!(!inferred.contains_key("R"));

    let signature = GenericSig {
        formals: vec!["T".to_string(), "R".to_string()],
        formal_bounds: vec![vec![], vec![]],
        receiver: None,
        params: vec![declared],
        ret: r,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let constrained = infer_generic_call_constraints_from_symbols(
        &crate::libraries::EmptySymbolSource,
        &signature,
        [(0, provisional, false)],
        None,
    );
    assert!(!constrained.bindings.contains_key("R"));
    assert!(constrained
        .bindings
        .values()
        .all(|binding| !binding.mentions_error()));
}

#[test]
fn typed_function_input_supplies_tightest_contravariant_binding() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let formal = Ty::ty_param("T", any);
    let signature = GenericSig {
        formals: vec!["T".to_string()],
        formal_bounds: vec![vec![any]],
        receiver: None,
        params: vec![Ty::fun(vec![formal], Ty::Unit)],
        ret: Ty::obj_args("test/C", &[formal]),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };

    let bindings = infer_generic_call_bindings_from_symbols(
        &crate::libraries::EmptySymbolSource,
        &signature,
        [(0, Ty::fun(vec![Ty::String], Ty::Unit), false)],
        None,
    );

    assert_eq!(bindings.get("T"), Some(&Ty::String));
}

#[test]
fn bound_violation_retains_the_prior_valid_argument_solution() {
    let formal = Ty::ty_param("T", Ty::Int);
    let signature = GenericSig {
        formals: vec!["T".to_string()],
        formal_bounds: vec![vec![Ty::Int]],
        receiver: None,
        params: vec![formal, formal],
        ret: Ty::obj_args("test/C", &[formal]),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };

    let inferred = infer_generic_call_constraints_from_symbols(
        &crate::libraries::EmptySymbolSource,
        &signature,
        [(0, Ty::Int, false), (1, Ty::String, false)],
        None,
    );

    assert_eq!(
        inferred.bound_violation,
        Some(GenericBoundViolation {
            argument: 1,
            expected: Ty::Int,
            actual: Ty::String,
        })
    );
}

#[test]
fn selected_bindings_publish_formals_implied_by_dependent_bounds() {
    let c = Ty::obj("sample/C");
    let t1 = Ty::ty_param("T1", c);
    let t2 = Ty::ty_param("T2", t1);
    let signature = GenericSig {
        formals: vec!["T1".to_string(), "T2".to_string()],
        formal_bounds: vec![vec![c], vec![t1]],
        receiver: None,
        params: vec![t2],
        ret: Ty::Unit,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let mut bindings = GSigBinds::new();

    merge_generic_bindings_from(
        None,
        &signature,
        0,
        &mut bindings,
        GSigBinds::from([("T2".to_string(), c)]),
    );

    assert_eq!(bindings.get("T1"), Some(&c));
    assert_eq!(bindings.get("T2"), Some(&c));
}

#[test]
fn argument_merge_completes_dependent_formal_before_intersection_bounds() {
    let uint = Ty::obj("kotlin/UInt");
    let comparable_uint = Ty::obj_args("kotlin/Comparable", &[uint]);
    let x = Ty::ty_param("X", comparable_uint);
    let t = Ty::ty_param("T", x);
    let signature = GenericSig {
        formals: vec!["T".to_string(), "X".to_string()],
        formal_bounds: vec![vec![x], vec![comparable_uint, uint]],
        receiver: None,
        params: vec![t],
        ret: Ty::Boolean,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let mut bindings = GSigBinds::new();

    merge_call_argument_bindings(
        &crate::libraries::EmptySymbolSource,
        &signature,
        0,
        &GSigBinds::new(),
        &mut bindings,
        GSigBinds::from([("T".to_string(), uint)]),
    );

    assert_eq!(bindings.get("T"), Some(&uint));
    assert_eq!(bindings.get("X"), Some(&uint));
}

#[test]
fn projected_receiver_binding_can_widen_to_a_nullable_argument_constraint() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let k = Ty::ty_param("K", any);
    let signature = GenericSig {
        formals: vec!["K".to_string()],
        formal_bounds: vec![vec![]],
        receiver: Some(Ty::out_projection(k)),
        params: vec![k],
        ret: k,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let receiver_bindings = GSigBinds::from([("K".to_string(), Ty::String)]);
    let mut bindings = receiver_bindings.clone();

    assert_eq!(
        formal_variance_in_type(
            &crate::libraries::EmptySymbolSource,
            Ty::obj_args("sample/Box", &[Ty::out_projection(k)]),
            "K",
        ),
        Some(crate::types::TypeVariance::Out),
    );

    merge_call_argument_bindings(
        &crate::libraries::EmptySymbolSource,
        &signature,
        0,
        &receiver_bindings,
        &mut bindings,
        GSigBinds::from([("K".to_string(), Ty::nullable(Ty::String))]),
    );

    assert_eq!(bindings.get("K"), Some(&Ty::nullable(Ty::String)));
}

#[test]
fn partially_specified_type_arguments_fix_only_written_positions() {
    let k = Ty::ty_param("K", Ty::nullable(Ty::obj("kotlin/Any")));
    let t = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
    let signature = GenericSig {
        formals: vec!["K".to_string(), "T".to_string()],
        formal_bounds: vec![vec![], vec![]],
        receiver: None,
        params: vec![Ty::fun(vec![k], t)],
        ret: Ty::obj_args("kotlin/Pair", &[k, t]),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };

    let written_then_hole = [Ty::Int, Ty::Error];
    let mut bindings = seeded_gsig_binds(&signature, &written_then_hole);
    merge_generic_bindings(
        &signature,
        written_then_hole.as_slice(),
        &mut bindings,
        GSigBinds::from([("K".to_string(), Ty::String), ("T".to_string(), Ty::Float)]),
    );
    assert_eq!(bindings.get("K"), Some(&Ty::Int));
    assert_eq!(bindings.get("T"), Some(&Ty::Float));

    let hole_then_written = [Ty::Error, Ty::String];
    let mut bindings = seeded_gsig_binds(&signature, &hole_then_written);
    merge_generic_bindings(
        &signature,
        hole_then_written.as_slice(),
        &mut bindings,
        GSigBinds::from([("K".to_string(), Ty::Int), ("T".to_string(), Ty::Float)]),
    );
    assert_eq!(bindings.get("K"), Some(&Ty::Int));
    assert_eq!(bindings.get("T"), Some(&Ty::String));
}

#[test]
fn constructor_result_inference_ignores_trailing_captured_arguments() {
    let owner = crate::types::type_name("sample/Outer$Inner");
    let formals = vec!["Own".to_string()];
    let expected = Ty::obj_args_name(owner, &[Ty::String, Ty::Int]);
    let mut bindings = GSigBinds::new();

    constrain_constructor_result(owner, &formals, Some(expected), &mut bindings);

    assert_eq!(bindings.get("Own"), Some(&Ty::String));
}

#[test]
fn constructor_result_seed_preserves_enclosing_declaration_type_variables() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let owner = crate::types::type_name("sample/State");
    let formals = vec!["class:S".to_string(), "class:A".to_string()];
    let caller_s = Ty::ty_param("class:S", any);
    let method_b = Ty::ty_param("method:B", any);
    let expected = Ty::obj_args_name(owner, &[caller_s, method_b]);
    let mut bindings = GSigBinds::new();

    seed_unbound_constructor_result_from_symbols(
        &crate::libraries::EmptySymbolSource,
        owner,
        &formals,
        &[vec![any], vec![any]],
        Some(expected),
        &mut bindings,
    );

    assert_eq!(bindings.get("class:S"), Some(&caller_s));
    assert_eq!(bindings.get("class:A"), Some(&method_b));
}

#[test]
fn null_argument_materializes_the_bottom_of_a_nullable_generic_parameter() {
    let formal = Ty::ty_param("T", Ty::obj("kotlin/Any"));
    let mut bindings = GSigBinds::new();

    unify_inferred_ty(Ty::nullable(formal), Ty::Null, &mut bindings);

    assert_eq!(bindings.get("T"), Some(&Ty::Nothing));
}

#[test]
fn callable_reference_expectation_keeps_inputs_fixed_and_postpones_results() {
    let t = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
    let r = Ty::ty_param("R", Ty::nullable(Ty::obj("kotlin/Any")));
    let signature = GenericSig {
        formals: vec!["T".to_string(), "R".to_string()],
        formal_bounds: vec![vec![], vec![]],
        receiver: None,
        params: vec![Ty::fun(vec![t], r)],
        ret: Ty::Unit,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let bindings = GSigBinds::from([("T".to_string(), Ty::String), ("R".to_string(), Ty::String)]);

    let expected = callable_reference_expected_bindings(
        &crate::libraries::EmptySymbolSource,
        &signature,
        0,
        0,
        &bindings,
        &std::collections::HashSet::new(),
    );

    assert_eq!(expected.get("T"), Some(&Ty::String));
    assert!(!expected.contains_key("R"));

    let selected = std::collections::HashSet::from(["R".to_string()]);
    let expected = callable_reference_expected_bindings(
        &crate::libraries::EmptySymbolSource,
        &signature,
        0,
        0,
        &bindings,
        &selected,
    );
    assert_eq!(expected.get("R"), Some(&Ty::String));
}

#[test]
fn callable_reference_expectation_keeps_invariant_expected_result_equalities() {
    let r = Ty::ty_param("R", Ty::nullable(Ty::obj("kotlin/Any")));
    let signature = GenericSig {
        formals: vec!["R".to_string()],
        formal_bounds: vec![vec![]],
        receiver: None,
        params: vec![Ty::fun(vec![], r)],
        ret: Ty::fun(vec![r], r),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let bindings = GSigBinds::from([("R".to_string(), Ty::String)]);
    let fixed = invariant_expected_result_formals(
        &crate::libraries::EmptySymbolSource,
        &signature,
        Ty::fun(vec![Ty::String], Ty::String),
        &bindings,
    );

    let expected = callable_reference_expected_bindings(
        &crate::libraries::EmptySymbolSource,
        &signature,
        0,
        0,
        &bindings,
        &fixed,
    );
    assert_eq!(expected.get("R"), Some(&Ty::String));
}

#[test]
fn projected_expected_result_does_not_widen_an_argument_binding() {
    let any = Ty::obj("kotlin/Any");
    let nullable_any = Ty::nullable(any);
    let x = Ty::ty_param("X", nullable_any);
    let signature = GenericSig {
        formals: vec!["X".to_string()],
        formal_bounds: vec![vec![]],
        receiver: None,
        params: vec![Ty::obj_args("test/Box", &[x])],
        ret: Ty::obj_args("test/Box", &[x]),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let expected = Ty::obj_args("test/Box", &[Ty::out_projection(nullable_any)]);
    let expected_bindings = GSigBinds::from([("X".to_string(), nullable_any)]);
    let mut bindings = GSigBinds::from([("X".to_string(), any)]);

    widen_invariant_expected_bindings(
        &crate::libraries::EmptySymbolSource,
        &signature,
        0,
        &mut bindings,
        &expected_bindings,
        expected,
        |actual, bound| actual == bound || bound == nullable_any,
    );

    assert_eq!(bindings.get("X"), Some(&any));
    assert!(invariant_expected_result_formals(
        &crate::libraries::EmptySymbolSource,
        &signature,
        expected,
        &expected_bindings,
    )
    .is_empty());
}

#[test]
fn whole_vararg_array_inference_descends_through_out_projection() {
    let k = Ty::ty_param("K", Ty::nullable(Ty::obj("kotlin/Any")));
    let v = Ty::ty_param("V", Ty::nullable(Ty::obj("kotlin/Any")));
    let signature = GenericSig {
        formals: vec!["K".to_string(), "V".to_string()],
        formal_bounds: vec![vec![], vec![]],
        receiver: None,
        params: vec![Ty::obj_args(
            "kotlin/Array",
            &[Ty::obj_args("kotlin/Pair", &[k, v])],
        )],
        ret: k,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let actual = Ty::obj_args(
        "kotlin/Array",
        &[Ty::out_projection(Ty::obj_args(
            "kotlin/Pair",
            &[Ty::String, Ty::Int],
        ))],
    );

    let bindings = infer_generic_call_bindings(&signature, [(0, actual, true)], Some(0));

    assert_eq!(bindings.get("K"), Some(&Ty::String));
    assert_eq!(bindings.get("V"), Some(&Ty::Int));

    let bindings = infer_generic_call_bindings_from_symbols(
        &crate::libraries::EmptySymbolSource,
        &signature,
        [(0, actual, true)],
        Some(0),
    );
    assert_eq!(bindings.get("K"), Some(&Ty::String));
    assert_eq!(bindings.get("V"), Some(&Ty::Int));
}

#[test]
fn inferred_dependency_materializes_an_otherwise_unobserved_bound_formal() {
    let any = Ty::obj("kotlin/Any");
    let e = Ty::ty_param("E", any);
    let signature = GenericSig {
        formals: vec!["T".to_string(), "E".to_string()],
        formal_bounds: vec![vec![Ty::obj_args("test/KFunction", &[e])], vec![any]],
        receiver: None,
        params: vec![Ty::obj_args("test/A", &[e])],
        ret: Ty::obj_args("kotlin/Pair", &[Ty::String, e]),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let mut bindings = GSigBinds::from([("E".to_string(), any)]);

    complete_dependency_instantiated_bound_bindings(&signature, &mut bindings, 0);

    assert_eq!(
        bindings.get("T"),
        Some(&Ty::obj_args("test/KFunction", &[any]))
    );

    let plain = GenericSig {
        formals: vec!["T".to_string()],
        formal_bounds: vec![vec![any]],
        receiver: None,
        params: vec![],
        ret: Ty::Unit,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let mut unconstrained = GSigBinds::new();
    complete_dependency_instantiated_bound_bindings(&plain, &mut unconstrained, 0);
    assert!(unconstrained.is_empty());
}

#[test]
fn captured_self_bound_keeps_denotable_caller_type_parameters() {
    let any = Ty::obj("kotlin/Any");
    let nullable_any = Ty::nullable(any);
    let data = Ty::ty_param("caller:U", any);
    let self_formal = Ty::ty_param(
        "owner:S",
        Ty::obj_args(
            "test/Entity",
            &[
                Ty::ty_param("owner:D", nullable_any),
                Ty::ty_param("owner:S", nullable_any),
            ],
        ),
    );
    let captured_self = Ty::star_projection(Ty::obj_args("test/Entity", &[data, self_formal]));
    let method = Ty::ty_param("method:T", captured_self);
    let signature = GenericSig {
        formals: vec!["method:T".to_string()],
        formal_bounds: vec![vec![captured_self]],
        receiver: Some(Ty::obj_args("test/Entity", &[data, captured_self])),
        params: vec![nullable_any],
        ret: method,
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let receiver_bindings = GSigBinds::from([
        ("owner:D".to_string(), data),
        ("owner:S".to_string(), captured_self),
    ]);
    let mut bindings = GSigBinds::new();

    complete_return_only_captured_receiver_bindings(
        &signature,
        &receiver_bindings,
        &mut bindings,
        0,
    );

    assert_eq!(
        bindings.get("method:T"),
        Some(&Ty::obj_args(
            "test/Entity",
            &[data, Ty::star_projection(nullable_any)],
        ))
    );
}

#[test]
fn expected_result_cannot_replace_a_recursive_declared_bound() {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let bound_formal = Ty::ty_param("T", any);
    let comparable = Ty::obj_args("kotlin/Comparable", &[bound_formal]);
    let formal = Ty::ty_param("T", comparable);
    let signature = GenericSig {
        formals: vec!["T".to_string()],
        formal_bounds: vec![vec![comparable]],
        receiver: None,
        params: vec![],
        ret: Ty::nullable(formal),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };

    assert_eq!(
        infer_generic_return_bindings(
            &signature,
            Ty::nullable(Ty::obj("test/Unrelated")),
            |actual, bound| actual == bound,
        ),
        None
    );
}

#[test]
fn expected_result_can_intersect_an_independent_declared_bound() {
    let bound = Ty::obj("test/Bound");
    let formal = Ty::ty_param("T", bound);
    let signature = GenericSig {
        formals: vec!["T".to_string()],
        formal_bounds: vec![vec![bound]],
        receiver: None,
        params: vec![],
        ret: Ty::nullable(formal),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let expected = Ty::nullable(Ty::obj("test/Expected"));

    assert_eq!(
        infer_generic_return_bindings(&signature, expected, |actual, bound| actual == bound),
        Some(GSigBinds::from([(
            "T".to_string(),
            Ty::obj("test/Expected")
        )]))
    );
}
