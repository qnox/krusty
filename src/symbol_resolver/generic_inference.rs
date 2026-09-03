//! Generic call and constructor constraint collection, solving, and substitution.

use super::*;

/// Whether `left` is the same callable parameter shape as `right` with a strict superset of
/// declaration constraints. Kotlin uses this after ordinary call-site applicability: for overloads
/// `<T : Comparable<T>> f(T)` and `<T : Comparable<T>, T : Number> f(T)`, an `Int` argument selects
/// the latter. Type-parameter identities are declaration-owned, so compare alpha-canonical shapes;
/// erase only their inline bounds while comparing parameter positions, then compare the complete
/// canonical bound sets by ordinal.
pub(crate) fn generic_signature_strictly_more_constrained(
    left: &GenericSig,
    right: &GenericSig,
) -> bool {
    if left.formals.len() != right.formals.len() {
        return false;
    }
    let neutral = Ty::nullable(Ty::obj("kotlin/Any"));
    let left_neutral = left
        .formals
        .iter()
        .cloned()
        .map(|formal| (formal, neutral))
        .collect::<GSigBinds>();
    let right_neutral = right
        .formals
        .iter()
        .cloned()
        .map(|formal| (formal, neutral))
        .collect::<GSigBinds>();
    let shape = |signature: &GenericSig, neutral: &GSigBinds| {
        (
            signature.receiver.map(|receiver| {
                crate::types::ty_canonicalize_params(
                    crate::types::ty_with_param_bounds(receiver, neutral),
                    &signature.formals,
                )
            }),
            signature
                .params
                .iter()
                .map(|parameter| {
                    crate::types::ty_canonicalize_params(
                        crate::types::ty_with_param_bounds(*parameter, neutral),
                        &signature.formals,
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    if shape(left, &left_neutral) != shape(right, &right_neutral) {
        return false;
    }

    let canonical_bounds = |signature: &GenericSig, ordinal: usize| {
        signature
            .formal_bounds
            .get(ordinal)
            .into_iter()
            .flatten()
            .map(|bound| crate::types::ty_canonicalize_params(*bound, &signature.formals))
            .collect::<Vec<_>>()
    };
    let mut strict = false;
    for ordinal in 0..left.formals.len() {
        let left_bounds = canonical_bounds(left, ordinal);
        let right_bounds = canonical_bounds(right, ordinal);
        if !right_bounds.iter().all(|bound| left_bounds.contains(bound)) {
            return false;
        }
        strict |= left_bounds
            .iter()
            .any(|bound| !right_bounds.contains(bound));
    }
    strict
}

/// Which declaration type-parameter positions were fixed by written call-site arguments.
///
/// Most internal inference entry points have no holes and can pass a prefix length. Source calls
/// with partially specified arguments pass their resolved argument slice instead: `Ty::Error` is
/// the parser/checker placeholder for `_` while overload selection is active, and therefore does
/// not fix that position. The placeholder never leaves resolution or enters checked FIR.
pub(crate) trait ExplicitTypeArgumentFixity: Copy {
    fn fixes(self, index: usize) -> bool;

    fn inferred_hole(self, _index: usize) -> bool {
        false
    }
}

impl ExplicitTypeArgumentFixity for usize {
    fn fixes(self, index: usize) -> bool {
        index < self
    }
}

impl ExplicitTypeArgumentFixity for &[Ty] {
    fn fixes(self, index: usize) -> bool {
        self.get(index)
            .is_some_and(|argument| *argument != Ty::Error)
    }

    fn inferred_hole(self, index: usize) -> bool {
        self.get(index) == Some(&Ty::Error)
    }
}

/// The type arguments of a constructed generic type INFERRED from a construction's argument types
/// (`Pair(1, 2)` → `[Int, Int]`, so `Pair(1, 2)` types as `Pair<Int, Int>`). Each of the type's formal
/// parameters (`ty.type_params`) is bound by unifying the matching-arity constructor's parsed generic
/// parameter signatures against `arg_tys`; an unbound formal defaults to `Any`. `None` when the type is
/// non-generic or no constructor carries a generic signature to unify.
pub(crate) fn infer_constructor_type_args(
    source: &dyn SymbolSource,
    owner: TypeName,
    ty: &crate::libraries::LibraryType,
    arg_tys: &[Ty],
    expected: Option<Ty>,
) -> Option<Vec<Ty>> {
    infer_constructor_type_args_for_formals(source, owner, ty, &ty.type_params, arg_tys, expected)
}

/// Constructor inference for the classifier's own declared formals. Provider-normalized source
/// inner classifiers may additionally expose captured enclosing formals on [`LibraryType`], because
/// receiver/member substitution needs the complete applied shape. Those captures are not constructor
/// type arguments; callers that own the source classifier boundary pass only its declared prefix.
pub(crate) fn infer_constructor_type_args_for_formals(
    source: &dyn SymbolSource,
    owner: TypeName,
    ty: &crate::libraries::LibraryType,
    type_params: &[String],
    arg_tys: &[Ty],
    expected: Option<Ty>,
) -> Option<Vec<Ty>> {
    if type_params.is_empty() {
        return None;
    }
    let mut binds = GSigBinds::new();
    for ctor in &ty.constructors {
        let Some(gsig) = &ctor.generic_sig else {
            continue;
        };
        if gsig.params.len() != arg_tys.len() {
            continue;
        }
        for (p, a) in gsig.params.iter().zip(arg_tys) {
            unify_inferred_ty(*p, *a, &mut binds);
        }
        break;
    }
    constrain_constructor_result(owner, type_params, expected, &mut binds);
    seed_unbound_constructor_result_from_symbols(
        source,
        owner,
        type_params,
        ty.type_param_bounds(),
        expected,
        &mut binds,
    );
    if binds.is_empty() {
        return None;
    }
    Some(
        type_params
            .iter()
            .map(|f| {
                binds
                    .get(f)
                    .copied()
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"))
            })
            .collect(),
    )
}

/// Seed every unbound constructor formal from an expected applied supertype, or none of them.
pub(crate) fn seed_unbound_constructor_result_from_symbols(
    source: &dyn SymbolSource,
    owner: TypeName,
    type_params: &[String],
    type_param_bounds: &[Vec<Ty>],
    expected: Option<Ty>,
    bindings: &mut GSigBinds,
) {
    let Some(expected) = expected.filter(|expected| *expected != Ty::Error) else {
        return;
    };
    let symbolic_arguments = type_params
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let bound = type_param_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(name, bound)
        })
        .collect::<Vec<_>>();
    let signature = GenericSig {
        formals: type_params.to_vec(),
        formal_bounds: type_param_bounds.to_vec(),
        receiver: None,
        params: Vec::new(),
        ret: Ty::obj_args_name(owner, &symbolic_arguments),
        return_policy: crate::libraries::GenericReturnPolicy::Exact,
    };
    let Some(seeds) = infer_generic_return_bindings_from_symbols(
        source,
        &signature,
        expected,
        |actual, bound| {
            crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(source),
                actual,
                bound,
            )
        },
    ) else {
        return;
    };
    if !type_params
        .iter()
        .all(|formal| bindings.contains_key(formal) || seeds.contains_key(formal))
    {
        return;
    }
    for formal in type_params {
        if let Some(&seed) = seeds.get(formal) {
            bindings.entry(formal.clone()).or_insert(seed);
        }
    }
}

/// Add the constructed value's expected type to the same inference bindings populated from
/// constructor arguments. Materialization happens only after both constraint directions have run.
pub(crate) fn constrain_constructor_result(
    owner: TypeName,
    type_params: &[String],
    expected: Option<Ty>,
    binds: &mut GSigBinds,
) {
    let Some(Ty::Obj(expected_owner, expected_args)) = expected.map(Ty::non_null) else {
        return;
    };
    // Source inner/local classifiers append captured enclosing arguments after their own declared
    // arguments. Constructor inference owns only `type_params`, so constrain that prefix and leave
    // the captured suffix to the classifier-application boundary. Ordinary classifiers have no
    // suffix and therefore continue to use the exact same path.
    if expected_owner != owner || expected_args.len() < type_params.len() {
        return;
    }
    for (formal, actual) in type_params.iter().zip(expected_args.iter().copied()) {
        // A star projection contributes only an existential read approximation. It is not an
        // equality constraint on the constructed classifier's argument: in
        // `consume(LoadConstant(0, SInt32))` where `consume` takes `LoadConstant<*, *>`, the value
        // arguments infer `Int` and `SInt32`; merging the stars' upper bounds would erase both to
        // `Any?`/`Any`. The applied result is checked against the projection after inference.
        if matches!(actual, Ty::StarProjection(_)) {
            continue;
        }
        // An enclosing generic call can provide a still-symbolic expectation (`A<OuterT>`). It is
        // useful when the constructor arguments provide no evidence, but it must not widen a
        // concrete argument constraint already collected for this classifier (`A(0)` is `A<Int>`,
        // even while the outer callable's `T` is unresolved). The outer solver subsequently binds
        // its own variable from this concrete constructor result.
        if binds.contains_key(formal) && actual.mentions_ty_param() {
            continue;
        }
        unify_inferred_ty(
            Ty::ty_param(formal, Ty::nullable(Ty::obj("kotlin/Any"))),
            actual.projection_inner().unwrap_or(actual),
            binds,
        );
    }
}

#[cfg(test)]
mod tests {
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
        collect_generic_identity_bindings(
            function,
            function,
            &["owner:T".to_string()],
            &mut identity,
        );
        assert_eq!(identity.get("owner:T"), Some(&t));
    }

    #[test]
    fn identity_evidence_does_not_bind_a_different_declaration_variable() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let declared = Ty::ty_param("callee:T", any);
        let actual = Ty::ty_param("caller:T", any);
        let mut identity = GSigBinds::new();

        collect_generic_identity_bindings(
            declared,
            actual,
            &["callee:T".to_string()],
            &mut identity,
        );

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
        let bindings =
            GSigBinds::from([("T".to_string(), Ty::String), ("R".to_string(), Ty::String)]);

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
}

/// Bind type variables by unifying a signature `Ty` (whose type variables are [`Ty::TyParam`]) against
/// an actual argument `Ty`.
fn unify_ty_impl(source: Option<&dyn SymbolSource>, sig: Ty, actual: Ty, binds: &mut GSigBinds) {
    // A failed/provisional subexpression contributes no inference constraint. Keep this guard at
    // every recursive entry so a function shell can still bind its valid receiver/parameter parts
    // while an unresolved result is ignored instead of becoming `R = Error`.
    if actual == Ty::Error {
        return;
    }
    match sig {
        // Projections change how a constraint is combined, not whether the nested declaration
        // shape participates in inference. In particular, `MutableCollection<in T>` must carry
        // the receiver's element type into overload selection; dropping it lets a later collection
        // argument rebind the element overload's `T` to `List<Int>`.
        Ty::InProjection(inner) | Ty::OutProjection(inner) => unify_ty_impl(
            source,
            *inner,
            actual.projection_inner().unwrap_or(actual),
            binds,
        ),
        // The bound carried by a star is a read approximation, not a written generic argument
        // from which call type parameters may be inferred.
        Ty::StarProjection(_) => {}
        Ty::TyParam(n, _) => {
            if actual.mentions_error() {
                return;
            }
            // A partially specialized candidate legitimately keeps an unconstrained formal in
            // place (`Mapper<Message, R>`). That is absence of evidence, not the binding `R = R`;
            // recording it would block the lambda result from later binding `R = String`.
            if matches!(actual, Ty::TyParam(actual, _) if actual == n) {
                return;
            }
            match binds.entry(n.to_string()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(actual);
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if *entry.get() == Ty::Nothing && actual != Ty::Nothing =>
                {
                    // `Nothing` is the bottom constraint produced by a diverging expression. It
                    // cannot freeze a type variable before another receiver/argument supplies a
                    // realizable type (`() -> Nothing` and `() -> String` jointly infer `String`).
                    entry.insert(actual);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        Ty::Fun(fsig) => {
            // A function parameter (`Function1<T, R>`) unifies against a lambda argument (`Ty::Fun`):
            // the parameter nodes bind the lambda's parameters and the return node binds its return, so
            // `map`'s `R` binds from the lambda body's type (`{ it * 2 }` → `Int`).
            if let Ty::Fun(afsig) = actual.non_null() {
                // A SUSPEND SAM parameter (`suspend CoroutineScope.() -> T`) erases to
                // `Function2<CoroutineScope, Continuation<T>, Object>` — the RESULT type parameter `T`
                // lives inside the trailing `Continuation<T>`, and the JVM return node is `Object`. The
                // lambda argument, however, ERASES its own `Continuation` type argument (to `Any`) and
                // carries its real result in `afsig.ret`. Binding `T` from the erased `Continuation<Any>`
                // would fix it to `Any` (`runBlocking { … } : Any`, losing the block's type); bind it from
                // `afsig.ret` instead, and skip the `Continuation` param so it isn't double-unified.
                let value_params: &[Ty] = match (fsig.suspend, fsig.params.last()) {
                    (true, Some(Ty::Obj(n, cargs)))
                        if crate::types::same(*n, crate::types::wk::continuation())
                            && !cargs.is_empty() =>
                    {
                        unify_ty_impl(source, cargs[0], afsig.ret, binds);
                        &fsig.params[..fsig.params.len() - 1]
                    }
                    _ => &fsig.params,
                };
                for (a, p) in value_params.iter().zip(afsig.params.iter()) {
                    unify_ty_impl(source, *a, *p, binds);
                }
                if let Ty::Nullable(inner) = fsig.ret {
                    unify_ty_impl(source, *inner, afsig.ret.non_null(), binds);
                } else {
                    unify_ty_impl(source, fsig.ret, afsig.ret, binds);
                }
            }
        }
        Ty::Obj(name, args) => {
            if let (Some(source), Ty::Fun(actual_function)) = (source, actual.non_null()) {
                let target = Ty::obj_args_name(name, args);
                if let Some(sam) = semantic_sam_signature(source, target) {
                    for (&parameter, &actual) in sam.params.iter().zip(&actual_function.params) {
                        unify_ty_impl(Some(source), parameter, actual, binds);
                    }
                    unify_ty_impl(Some(source), sam.ret, actual_function.ret, binds);
                    return;
                }
            }
            // Unify the type arguments positionally against the actual's carried arguments, if any.
            // The outer classifiers must denote the same type: matching `Array<T>` against
            // `Wrapper<Int>` by argument position invents `T = Int`. Vararg calls unwrap their array
            // declaration explicitly in `infer_generic_call_bindings`; ordinary unification never does.
            let mut actual = actual.projection_inner().unwrap_or(actual).non_null();
            while let (Some(_), Ty::TyParam(_, bound)) = (source, actual) {
                actual = bound.projection_inner().unwrap_or(*bound).non_null();
            }
            let projected = match actual {
                Ty::Obj(actual_name, _) if name == actual_name => Some(actual),
                Ty::Obj(_, _) => source.and_then(|source| {
                    receiver_hierarchy(source, actual)
                        .into_iter()
                        .map(|(ty, _)| ty)
                        .find(|ty| {
                            ty.obj_internal()
                                .is_some_and(|actual_name| name == actual_name)
                        })
                }),
                _ => None,
            };
            if let Some(Ty::Obj(_, targs)) = projected {
                for (a, t) in args.iter().zip(targs.iter()) {
                    unify_ty_impl(source, *a, *t, binds);
                }
            }
        }
        // Nullability/flexibility wraps the same generic shape; it does not hide type variables
        // inside that shape. Java signatures routinely expose this as `Class<A>!` and `A!`.
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => {
            // A null-only value is the bottom inhabitant of `T?`, so it materializes the wrapped
            // inference variable as non-null `Nothing`. Other nullable values constrain the same
            // wrapped variable from their non-null component (`String?` -> `T = String`).
            unify_ty_impl(source, *inner, nullable_generic_actual(actual), binds);
        }
        _ => {}
    }
}

pub(crate) fn unify_ty(sig: Ty, actual: Ty, binds: &mut GSigBinds) {
    unify_ty_impl(None, sig, actual, binds);
}

/// Unify a declaration shape against an actual source type, projecting the actual through its applied
/// supertype graph when the outer classifiers differ. This is how `KSerializer<Foo>` constrains the
/// `T` in `DeserializationStrategy<T>`; providers publish only direct metadata and core owns the walk.
pub(crate) fn unify_ty_from_symbols(
    source: &dyn SymbolSource,
    sig: Ty,
    actual: Ty,
    binds: &mut GSigBinds,
) {
    let sig = if matches!(actual.non_null(), Ty::Fun(_)) {
        super::declared_function_type(source, sig).unwrap_or(sig)
    } else {
        sig
    };
    unify_ty_impl(Some(source), sig, actual, binds);
}

/// Record declaration-owned type variables that are instantiated by the identical symbolic type
/// at a call site. Ordinary unification deliberately ignores `T` against `T`: during postponed
/// inference that pair is not a useful constraint and must not block a later concrete `T = String`.
/// It is nevertheless evidence that a selected recursive/self call has a complete identity
/// substitution. Keep that distinction explicit and publish these bindings only after all useful
/// receiver, argument, and expected-result constraints have been solved.
pub(crate) fn collect_generic_identity_bindings(
    declared: Ty,
    actual: Ty,
    formals: &[String],
    bindings: &mut GSigBinds,
) {
    match (declared, actual) {
        (Ty::TyParam(declared, _), actual @ Ty::TyParam(found, _))
            if declared == found && formals.iter().any(|formal| formal == declared) =>
        {
            bindings.entry(declared.to_string()).or_insert(actual);
        }
        (Ty::InProjection(declared), Ty::InProjection(actual))
        | (Ty::OutProjection(declared), Ty::OutProjection(actual))
        | (Ty::Nullable(declared), Ty::Nullable(actual))
        | (Ty::PlatformNullable(declared), Ty::PlatformNullable(actual)) => {
            collect_generic_identity_bindings(*declared, *actual, formals, bindings);
        }
        (Ty::Nullable(declared) | Ty::PlatformNullable(declared), actual) if !null_only(actual) => {
            collect_generic_identity_bindings(*declared, actual.non_null(), formals, bindings);
        }
        (Ty::Obj(declared_name, declared_args), Ty::Obj(actual_name, actual_args))
            if declared_name == actual_name =>
        {
            for (&declared, &actual) in declared_args.iter().zip(actual_args) {
                collect_generic_identity_bindings(declared, actual, formals, bindings);
            }
        }
        (Ty::Fun(declared), Ty::Fun(actual))
            if declared.params.len() == actual.params.len()
                && declared.context_count == actual.context_count
                && declared.has_receiver == actual.has_receiver
                && declared.suspend == actual.suspend =>
        {
            for (&declared, &actual) in declared.params.iter().zip(&actual.params) {
                collect_generic_identity_bindings(declared, actual, formals, bindings);
            }
            collect_generic_identity_bindings(declared.ret, actual.ret, formals, bindings);
        }
        _ => {}
    }
}

pub(crate) fn inference_actual(actual: Ty) -> Ty {
    if actual == Ty::Null {
        Ty::nullable(Ty::Nothing)
    } else {
        actual
    }
}

fn null_only(actual: Ty) -> bool {
    actual == Ty::Null || matches!(actual, Ty::Nullable(inner) if *inner == Ty::Nothing)
}

fn nullable_generic_actual(actual: Ty) -> Ty {
    if null_only(actual) {
        Ty::Nothing
    } else {
        actual.non_null()
    }
}

pub(crate) fn merge_inferred_ty(current: Option<Ty>, actual: Ty) -> Ty {
    let actual = inference_actual(actual);
    let Some(current) = current else {
        return actual;
    };
    if actual == Ty::Nothing {
        current
    } else if current == Ty::Nothing {
        actual
    } else if current == actual {
        current
    } else if matches!(actual, Ty::Nullable(inner) if *inner == Ty::Nothing) {
        Ty::nullable(current)
    } else if matches!(current, Ty::Nullable(inner) if *inner == Ty::Nothing) {
        Ty::nullable(actual)
    } else if current.non_null() == actual.non_null() {
        Ty::nullable(current.non_null())
    } else {
        let any = Ty::obj("kotlin/Any");
        if current.is_nullable() || actual.is_nullable() {
            Ty::nullable(any)
        } else {
            any
        }
    }
}

/// Merge another constraint set into one callable's bindings. Written type arguments are fixed;
/// inferred receiver/argument/result evidence joins through the same bottom/nullability rules.
pub(crate) fn merge_generic_bindings(
    signature: &GenericSig,
    explicit_type_arguments: impl ExplicitTypeArgumentFixity,
    bindings: &mut GSigBinds,
    inferred: GSigBinds,
) {
    merge_generic_bindings_from(None, signature, explicit_type_arguments, bindings, inferred);
}

/// Bindings safe to apply before selecting a postponed callable-reference argument.
///
/// Contravariant input variables are already useful expectations for selecting the referenced
/// callable. Covariant result variables remain inference outputs until that target contributes its
/// result constraint; fixing them from an earlier ordinary argument would reject a valid reference
/// instead of joining both lower bounds. Explicit type arguments are fixed in every position.
pub(crate) fn callable_reference_expected_bindings(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    parameter: usize,
    explicit_type_arguments: impl ExplicitTypeArgumentFixity,
    bindings: &GSigBinds,
    selected_by_prior_callable_references: &std::collections::HashSet<String>,
) -> GSigBinds {
    let Some(shape @ Ty::Fun(_)) = signature.params.get(parameter).copied().map(Ty::non_null)
    else {
        return bindings.clone();
    };
    let mut expected = bindings.clone();
    for (index, formal) in signature.formals.iter().enumerate() {
        if !explicit_type_arguments.fixes(index)
            && !selected_by_prior_callable_references.contains(formal)
            && formal_variance_in_type(source, shape, formal)
                == Some(crate::types::TypeVariance::Out)
        {
            expected.remove(formal);
        }
    }
    expected
}

/// Type variables fixed by an invariant expected-result constraint.
///
/// These bindings are equalities, not provisional lower bounds. Postponed callable-reference
/// selection may discard a covariant result binding learned from another value argument so the
/// reference can contribute its own result type, but it must retain an equality imposed through an
/// invariant return constructor (`Pair<T, R?>` expected as `Pair<Int?, String?>`).
pub(crate) fn invariant_expected_result_formals(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    expected: Ty,
    expected_bindings: &GSigBinds,
) -> std::collections::HashSet<String> {
    let Some(declared) = return_shape_at_expected_owner(source, signature.ret, expected) else {
        return std::collections::HashSet::new();
    };
    signature
        .formals
        .iter()
        .filter(|formal| {
            expected_bindings.get(*formal).is_some()
                && formal_has_unprojected_expected_occurrence(declared, expected, formal)
                && formal_variance_in_type(source, declared, formal)
                    == Some(crate::types::TypeVariance::Invariant)
        })
        .cloned()
        .collect()
}

/// Result formals whose matching expected occurrence is not a use-site projection. These may be
/// used to contextualize a postponed nested producer before its argument constraint is materialized;
/// a star/`in`/`out` expectation remains only an upper/lower bound and must not become an equality.
pub(crate) fn unprojected_expected_result_formals(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    expected: Ty,
    expected_bindings: &GSigBinds,
) -> std::collections::HashSet<String> {
    let Some(declared) = return_shape_at_expected_owner(source, signature.ret, expected) else {
        return std::collections::HashSet::new();
    };
    signature
        .formals
        .iter()
        .filter(|formal| {
            expected_bindings.get(*formal).is_some()
                && formal_has_unprojected_expected_occurrence(declared, expected, formal)
        })
        .cloned()
        .collect()
}

/// Whether at least one occurrence of `formal` in the declared result is paired with an
/// unprojected expected type. Inference bindings have already stripped use-site projections, so
/// inspecting the bound value cannot distinguish invariant `Box<Any?>` from `Box<out Any?>`.
fn formal_has_unprojected_expected_occurrence(declared: Ty, expected: Ty, formal: &str) -> bool {
    match declared {
        Ty::TyParam(name, _) if name == formal => expected.projection_inner().is_none(),
        Ty::Obj(declared_name, declared_args) => match expected.non_null() {
            Ty::Obj(expected_name, expected_args)
                if declared_name == expected_name && declared_args.len() == expected_args.len() =>
            {
                declared_args
                    .iter()
                    .zip(expected_args)
                    .any(|(&declared, &expected)| {
                        formal_has_unprojected_expected_occurrence(declared, expected, formal)
                    })
            }
            _ => false,
        },
        Ty::Fun(declared) => {
            match expected.non_null() {
                Ty::Fun(expected)
                    if declared.params.len() == expected.params.len()
                        && declared.context_count == expected.context_count
                        && declared.has_receiver == expected.has_receiver
                        && declared.suspend == expected.suspend =>
                {
                    declared.params.iter().zip(expected.params.iter()).any(
                        |(&declared, &expected)| {
                            formal_has_unprojected_expected_occurrence(declared, expected, formal)
                        },
                    ) || formal_has_unprojected_expected_occurrence(
                        declared.ret,
                        expected.ret,
                        formal,
                    )
                }
                _ => false,
            }
        }
        Ty::Nullable(declared) | Ty::PlatformNullable(declared) => {
            formal_has_unprojected_expected_occurrence(*declared, expected.non_null(), formal)
        }
        Ty::InProjection(declared) | Ty::OutProjection(declared) => {
            expected.projection_inner().is_some_and(|expected| {
                formal_has_unprojected_expected_occurrence(*declared, expected, formal)
            })
        }
        _ => false,
    }
}

/// [`merge_generic_bindings`] with the symbol source its completion steps need. Re-solving a
/// binding against a recursive bound is a hierarchy question, so a caller that has a source passes
/// it; one that does not gets exactly the previous behaviour.
pub(crate) fn merge_generic_bindings_from(
    source: Option<&dyn SymbolSource>,
    signature: &GenericSig,
    explicit_type_arguments: impl ExplicitTypeArgumentFixity,
    bindings: &mut GSigBinds,
    inferred: GSigBinds,
) {
    bindings.retain(|_, actual| !actual.mentions_error());
    for (formal, actual) in inferred {
        if actual.mentions_error() {
            continue;
        }
        let Some(formal_index) = signature
            .formals
            .iter()
            .position(|declared| declared == &formal)
        else {
            // A member signature can mention variables owned by its receiver class. They are
            // already substituted from the applied receiver and are constraints on an enclosing
            // call, not inference variables declared by this callable. In particular, checking
            // `Buildee<T>.yield(Local())` inside a postponed builder lambda must leave the receiver's
            // symbolic `T` intact so the checker can propagate `T = Local` to `build`.
            continue;
        };
        if explicit_type_arguments.fixes(formal_index) {
            continue;
        }
        let merged = bindings.get(&formal).copied().map_or_else(
            || merge_inferred_ty(None, actual),
            |current| merge_inferred_ty_from_symbols(source, current, actual),
        );
        bindings.insert(formal, merged);
    }
    // Join bottom bindings against `where`-clause subtype constraints (`ifBlank { null }` binds
    // `R := Nothing?` under `C : R`; the solution is `R = C?`). Here, at the common merge funnel,
    // so every selection, realization, and lambda-expectation path sees the same completed map.
    complete_bottom_constraint_bindings(signature, bindings, explicit_type_arguments);
    complete_dependent_bound_bindings(signature, bindings, explicit_type_arguments);
    resolve_bound_violating_bindings(source, signature, bindings, explicit_type_arguments);
}

/// Merge constraints contributed by an expected call result. These are upper bounds: they can fill
/// an otherwise unbound formal, but cannot overwrite a lower bound already established by a receiver
/// or value argument. An incompatible pair makes the specialized result inapplicable later.
pub(crate) fn merge_generic_upper_bindings(
    signature: &GenericSig,
    explicit_type_arguments: impl ExplicitTypeArgumentFixity,
    bindings: &mut GSigBinds,
    inferred: GSigBinds,
    _admits: impl FnMut(Ty, Ty) -> bool,
) {
    for (formal, upper) in inferred {
        let Some(formal_index) = signature
            .formals
            .iter()
            .position(|declared| declared == &formal)
        else {
            continue;
        };
        if explicit_type_arguments.fixes(formal_index) {
            continue;
        }
        let selected = bindings.get(&formal).copied().unwrap_or(upper);
        bindings.insert(formal, selected);
    }
}

/// Merge value-argument constraints with receiver evidence. A widened binding is retained only when
/// the already-applied receiver remains assignable to the receiver shape under that binding. This
/// preserves invariant receiver equality while allowing covariant receiver occurrences to contribute
/// a lower bound (`Producer<String>` plus `Any` may infer `T = Any`).
pub(crate) fn merge_call_argument_bindings(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    explicit_type_arguments: impl ExplicitTypeArgumentFixity,
    receiver_bindings: &GSigBinds,
    bindings: &mut GSigBinds,
    inferred: GSigBinds,
) {
    for (formal, actual) in inferred {
        let Some(formal_index) = signature
            .formals
            .iter()
            .position(|declared| declared == &formal)
        else {
            continue;
        };
        if explicit_type_arguments.fixes(formal_index) {
            continue;
        }
        let receiver = receiver_bindings.get(&formal).copied();
        let merged = match receiver {
            Some(receiver)
                if receiver != Ty::Nothing
                    && !matches!(receiver, Ty::Nullable(inner) if *inner == Ty::Nothing) =>
            {
                let variance = signature
                    .receiver
                    .and_then(|receiver_shape| {
                        formal_variance_in_type(source, receiver_shape, &formal)
                    })
                    .unwrap_or(crate::types::TypeVariance::Invariant);
                let candidate = match variance {
                    crate::types::TypeVariance::Out => {
                        merge_inferred_ty_from_symbols(Some(source), receiver, actual)
                    }
                    crate::types::TypeVariance::In => actual,
                    crate::types::TypeVariance::Invariant => receiver,
                };
                let candidate_binding =
                    std::collections::HashMap::from([(formal.clone(), candidate)]);
                let receiver_shape = signature
                    .receiver
                    .map(|receiver| ty_subst_keep_unbound(receiver, &candidate_binding))
                    .unwrap_or(receiver);
                if variance == crate::types::TypeVariance::Out
                    || crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        &SourceOracle(source),
                        signature
                            .receiver
                            .map(|declared| ty_subst_keep_unbound(declared, receiver_bindings))
                            .unwrap_or(receiver),
                        receiver_shape,
                    )
                {
                    candidate
                } else {
                    receiver
                }
            }
            _ => merge_inferred_ty(bindings.get(&formal).copied(), actual),
        };
        bindings.insert(formal, merged);
    }
    // Direct declaration dependencies are stronger than an upper bound's erased/applied
    // classifier. With `<T : X, X : Comparable<UInt>> where X : UInt`, an argument `T = UInt`
    // entails `X = UInt`; leaving X open until applied-supertype solving instead binds it to
    // `Comparable<UInt>`, which satisfies only one side of the intersection and rejects the call.
    complete_dependent_bound_bindings(signature, bindings, explicit_type_arguments);
}

pub(crate) fn formal_variance_in_type(
    source: &dyn SymbolSource,
    shape: Ty,
    formal: &str,
) -> Option<crate::types::TypeVariance> {
    use crate::types::TypeVariance;

    fn compose(outer: TypeVariance, inner: TypeVariance) -> TypeVariance {
        match (outer, inner) {
            (TypeVariance::Invariant, _) | (_, TypeVariance::Invariant) => TypeVariance::Invariant,
            (TypeVariance::Out, variance) => variance,
            (TypeVariance::In, TypeVariance::Out) => TypeVariance::In,
            (TypeVariance::In, TypeVariance::In) => TypeVariance::Out,
        }
    }

    fn merge(left: Option<TypeVariance>, right: Option<TypeVariance>) -> Option<TypeVariance> {
        match (left, right) {
            (None, variance) | (variance, None) => variance,
            (Some(left), Some(right)) if left == right => Some(left),
            (Some(_), Some(_)) => Some(TypeVariance::Invariant),
        }
    }

    // Nullability is orthogonal to variance: `R?` has the same occurrence variance as `R`.
    // Inspect the normalized shape here as well as in the recursive object/function branches.
    if matches!(shape.non_null(), Ty::TyParam(name, _) if name == formal) {
        return Some(TypeVariance::Out);
    }
    match shape.non_null() {
        Ty::InProjection(inner) => formal_variance_in_type(source, *inner, formal)
            .map(|nested| compose(TypeVariance::In, nested)),
        Ty::OutProjection(inner) => formal_variance_in_type(source, *inner, formal)
            .map(|nested| compose(TypeVariance::Out, nested)),
        Ty::StarProjection(_) => None,
        Ty::Obj(owner, arguments) => {
            let classifier = source.classifier(owner);
            arguments
                .iter()
                .enumerate()
                .fold(None, |found, (index, argument)| {
                    let nested = formal_variance_in_type(source, *argument, formal).map(|nested| {
                        // A use-site projection supplies the effective variance of this argument
                        // even when the classifier is invariant or its declaration metadata is not
                        // available. For an unprojected argument, compose with declaration-site
                        // variance as usual.
                        if matches!(argument, Ty::InProjection(_) | Ty::OutProjection(_)) {
                            nested
                        } else {
                            compose(
                                classifier
                                    .as_ref()
                                    .and_then(|classifier| {
                                        classifier.type_param_variances.get(index).copied()
                                    })
                                    .unwrap_or(TypeVariance::Invariant),
                                nested,
                            )
                        }
                    });
                    merge(found, nested)
                })
        }
        Ty::Fun(function) => {
            let parameters = function.params.iter().fold(None, |found, parameter| {
                let nested = formal_variance_in_type(source, *parameter, formal)
                    .map(|variance| compose(TypeVariance::In, variance));
                merge(found, nested)
            });
            merge(
                parameters,
                formal_variance_in_type(source, function.ret, formal),
            )
        }
        _ => None,
    }
}

pub(super) fn unify_inferred_ty_impl(
    source: Option<&dyn SymbolSource>,
    sig: Ty,
    actual: Ty,
    binds: &mut GSigBinds,
) {
    if actual == Ty::Error {
        return;
    }
    match sig {
        Ty::InProjection(inner) | Ty::OutProjection(inner) => unify_inferred_ty_impl(
            source,
            *inner,
            actual.projection_inner().unwrap_or(actual),
            binds,
        ),
        Ty::StarProjection(_) => {}
        Ty::TyParam(name, _) => {
            if actual.mentions_error() {
                return;
            }
            // A postponed expression may already carry the very variable we are collecting
            // constraints for (`FT` against `FT`). That is an identity, not evidence. Recording it
            // makes the later useful constraint (`FT` against `UserKlass`) join with a symbolic
            // self-reference and collapse to `Any`.
            if matches!(actual, Ty::TyParam(actual_name, _) if actual_name == name) {
                return;
            }
            match binds.entry(name.to_string()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(inference_actual(
                        actual.projection_inner().unwrap_or(actual),
                    ));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.insert(merge_inferred_ty_from_symbols(
                        source,
                        *entry.get(),
                        actual.projection_inner().unwrap_or(actual),
                    ));
                }
            }
        }
        Ty::Fun(signature) => {
            if let Ty::Fun(actual) = actual.non_null() {
                for (parameter, actual) in signature.params.iter().zip(actual.params.iter()) {
                    unify_inferred_ty_impl(source, *parameter, *actual, binds);
                }
                unify_inferred_ty_impl(source, signature.ret, actual.ret, binds);
            }
        }
        Ty::Obj(name, arguments) => {
            let actual = actual.projection_inner().unwrap_or(actual).non_null();
            let projected = match actual {
                Ty::Obj(actual_name, _) if name == actual_name => Some(actual),
                Ty::Obj(_, _) => source.and_then(|source| {
                    receiver_hierarchy(source, actual)
                        .into_iter()
                        .map(|(ty, _)| ty)
                        .find(|ty| {
                            ty.obj_internal()
                                .is_some_and(|actual_name| name == actual_name)
                        })
                }),
                _ => None,
            };
            if let Some(Ty::Obj(_, actual_arguments)) = projected {
                for (argument, actual) in arguments.iter().zip(actual_arguments.iter()) {
                    unify_inferred_ty_impl(source, *argument, *actual, binds);
                }
            }
        }
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => {
            unify_inferred_ty_impl(source, *inner, nullable_generic_actual(actual), binds);
        }
        _ => {}
    }
}

pub(super) fn merge_inferred_ty_from_symbols(
    source: Option<&dyn SymbolSource>,
    current: Ty,
    actual: Ty,
) -> Ty {
    let current = current.projection_inner().unwrap_or(current);
    let actual = actual.projection_inner().unwrap_or(actual);
    if current.non_null() == actual.non_null() {
        let base = current.non_null();
        return if current.is_nullable() || actual.is_nullable() {
            Ty::nullable(base)
        } else if matches!(current, Ty::PlatformNullable(_))
            || matches!(actual, Ty::PlatformNullable(_))
        {
            Ty::platform_nullable(base)
        } else {
            base
        };
    }
    // Function values with the same input shape have a function-shaped common supertype: inputs
    // stay fixed (function parameters are contravariant) and results contribute lower bounds
    // covariantly. Erasing the whole pair to `Any` makes the selected result non-callable before
    // checked FIR (`choose({ First() }, { Second() })` must remain `() -> Result`). When the input
    // shapes differ, leave the join to the nominal fallback rather than inventing a parameter meet.
    if let (Ty::Fun(current_function), Ty::Fun(actual_function)) =
        (current.non_null(), actual.non_null())
    {
        if current_function.params == actual_function.params
            && current_function.context_count == actual_function.context_count
            && current_function.has_receiver == actual_function.has_receiver
            && current_function.suspend == actual_function.suspend
        {
            let result =
                merge_inferred_ty_from_symbols(source, current_function.ret, actual_function.ret);
            let function = Ty::fun_with_shape(
                current_function.params.clone(),
                result,
                current_function.context_count,
                current_function.has_receiver,
                current_function.suspend,
            );
            return if current.is_nullable() || actual.is_nullable() {
                Ty::nullable(function)
            } else if matches!(current, Ty::PlatformNullable(_))
                || matches!(actual, Ty::PlatformNullable(_))
            {
                Ty::platform_nullable(function)
            } else {
                function
            };
        }
    }
    if let Some(source) = source {
        let oracle = SourceOracle(source);
        let context = crate::assignable::TyCtx::new();
        let current_to_actual =
            crate::assignable::is_assignable(&context, &oracle, current, actual);
        let actual_to_current =
            crate::assignable::is_assignable(&context, &oracle, actual, current);
        if current_to_actual && !actual_to_current {
            return actual;
        }
        if actual_to_current && !current_to_actual {
            return current;
        }
    }
    let merged = merge_inferred_ty(Some(current), actual);
    let Some(source) = source else { return merged };
    if !merged.is_erased_top() || !current.is_reference() || !actual.is_reference() {
        return merged;
    }
    let left = receiver_hierarchy(source, current.non_null());
    let right = receiver_hierarchy(source, actual.non_null());
    let mut common = left
        .iter()
        .flat_map(|(left_ty, left_depth)| {
            right.iter().filter_map(move |(right_ty, right_depth)| {
                let left_name = left_ty.obj_internal()?;
                let right_name = right_ty.obj_internal()?;
                (left_name == right_name).then_some((
                    left_depth + right_depth,
                    if left_ty == right_ty {
                        *left_ty
                    } else {
                        Ty::obj_name(left_name)
                    },
                ))
            })
        })
        .collect::<Vec<_>>();
    let Some(nearest) = common.iter().map(|(distance, _)| *distance).min() else {
        return merged;
    };
    common.retain(|(distance, ty)| *distance == nearest && !ty.is_erased_top());
    common.dedup_by(|left, right| left.1 == right.1);
    let [(_, common)] = common.as_slice() else {
        return merged;
    };
    if current.is_nullable() || actual.is_nullable() {
        Ty::nullable(common.non_null())
    } else {
        *common
    }
}

pub(crate) fn unify_inferred_ty(sig: Ty, actual: Ty, binds: &mut GSigBinds) {
    unify_inferred_ty_impl(None, sig, actual, binds);
}

pub(crate) fn unify_inferred_ty_with_source(
    source: &dyn SymbolSource,
    sig: Ty,
    actual: Ty,
    binds: &mut GSigBinds,
) {
    unify_inferred_ty_impl(Some(source), sig, actual, binds);
}

pub(crate) fn infer_generic_bindings(
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, Ty)>,
) -> GSigBinds {
    let mut binds = GSigBinds::new();
    for (parameter, actual) in actuals {
        if let Some(&shape) = generic_sig.params.get(parameter) {
            unify_inferred_ty(shape, actual, &mut binds);
        }
    }
    binds
}

/// Infer a callable's type parameters from source arguments already mapped to declaration slots.
/// A plain argument at a vararg slot constrains the ARRAY ELEMENT; a spread/whole-array argument
/// constrains the array declaration itself. This is the only legal place to unwrap a vararg array —
/// structural type unification deliberately requires matching outer classifiers.
pub(crate) fn infer_generic_call_bindings(
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> GSigBinds {
    let mut binds = GSigBinds::new();
    for (parameter, actual, whole_array) in actuals {
        let Some(mut shape) = generic_sig.params.get(parameter).copied() else {
            continue;
        };
        if vararg_index == Some(parameter) && !whole_array {
            let Some(element) = shape.array_elem() else {
                continue;
            };
            shape = element;
        }
        unify_inferred_ty(shape, actual, &mut binds);
    }
    binds
}

pub(crate) fn infer_generic_call_constraints_from_symbols(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> InferredCallBindings {
    infer_generic_call_constraints_with_argument_ordinals(
        source,
        generic_sig,
        actuals
            .into_iter()
            .enumerate()
            .map(|(argument, (parameter, actual, whole_array))| {
                (argument, parameter, actual, whole_array)
            }),
        vararg_index,
    )
}

/// The same constraint operation with an explicit source-argument ordinal. A non-denotable
/// conditional contributes each branch type as a lower constraint while every constituent still
/// belongs to the one source argument for diagnostics.
pub(crate) fn infer_generic_call_constraints_with_argument_ordinals(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> InferredCallBindings {
    let mut constraints = CallInferenceConstraints::default();
    for (argument, parameter, actual, whole_array) in actuals {
        let Some(mut shape) = generic_sig.params.get(parameter).copied() else {
            continue;
        };
        if vararg_index == Some(parameter)
            && (!whole_array || actual.non_null().array_elem().is_none())
        {
            let Some(element) = shape.array_elem() else {
                continue;
            };
            shape = element;
        }
        // Callable references and function values constrain a functional-interface parameter
        // through its single abstract method, not through their nominal `KFunctionN`/`FunctionN`
        // carrier. This belongs at the constraint seam: callers that need the full upper/lower
        // relation and callers that need only solved bindings must observe the same inference.
        if matches!(actual.non_null(), Ty::Fun(_)) {
            if let Some(sam) = semantic_sam_signature(source, shape) {
                shape = Ty::fun_with_shape(
                    sam.params,
                    sam.ret,
                    sam.context_count,
                    sam.has_receiver,
                    sam.suspend,
                );
            }
        }
        collect_call_inference_constraints(
            source,
            shape,
            actual,
            ConstraintPosition::Lower,
            argument,
            &mut constraints,
        );
    }
    constraints.solve(source, generic_sig)
}

pub(crate) fn infer_generic_call_bindings_from_symbols(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> GSigBinds {
    let mut inferred =
        infer_generic_call_constraints_from_symbols(source, generic_sig, actuals, vararg_index);
    let tightest_upper = inferred.tightest_upper_bindings(source);
    for formal in inferred.upper_only {
        let binding = tightest_upper.get(&formal).copied().unwrap_or(Ty::Nothing);
        inferred.bindings.entry(formal).or_insert(binding);
    }
    inferred.bindings
}

/// Specialize a fully typed call against one semantic declaration signature.
///
/// Signature-graph evaluation reaches this operation after postponed arguments have acquired their
/// semantic types. It must use the same constraint solver and source hierarchy as ordinary overload
/// selection: comparing an argument directly with a `TyParam` only checks its erased inline bound,
/// loses the inferred substitution, and can reject a source subtype such as `Token : Marker`.
pub(crate) fn specialize_typed_call_signature(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    arguments: &[Ty],
    type_arguments: &[Ty],
    expected: Option<Ty>,
) -> Option<(Vec<Ty>, Ty)> {
    if arguments.len() != signature.params.len() || type_arguments.len() > signature.formals.len() {
        return None;
    }

    let mut bindings = seeded_gsig_binds(signature, type_arguments);
    let inferred = infer_generic_call_bindings_from_symbols(
        source,
        signature,
        arguments
            .iter()
            .copied()
            .enumerate()
            .map(|(parameter, actual)| (parameter, actual, true)),
        None,
    );
    merge_call_argument_bindings(
        source,
        signature,
        type_arguments,
        &GSigBinds::new(),
        &mut bindings,
        inferred,
    );
    if let Some(expected) = expected {
        if let Some(inferred) = infer_generic_return_bindings_from_symbols(
            source,
            signature,
            expected,
            |actual, bound| resolution_subtype(source, actual, bound),
        ) {
            for (formal, actual) in inferred {
                bindings.entry(formal).or_insert(actual);
            }
        }
    }
    if !generic_bindings_satisfy_bounds(signature, &bindings, |actual, bound| {
        resolution_subtype(source, actual, bound)
    }) {
        return None;
    }

    let parameters = signature
        .params
        .iter()
        .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
        .collect::<Vec<_>>();
    if !parameters
        .iter()
        .zip(arguments)
        .all(|(parameter, actual)| resolution_subtype(source, *actual, *parameter))
    {
        return None;
    }
    Some((parameters, ty_subst_keep_unbound(signature.ret, &bindings)))
}

/// Enforce Kotlin's `@OnlyInputTypes` inference policy for selected declaration formals.
///
/// Ordinary lower-bound solving may synthesize a common supertype which was never present in the
/// call (`Int` from the receiver plus `String?` from an argument becomes `Any?`). For an
/// `@OnlyInputTypes` formal that is forbidden: its solution must be a proper type extracted from one
/// individual input occurrence. We therefore enumerate the receiver, each mapped value argument,
/// and the expected result independently, then retain the first complete substitution which admits
/// every input. This is provider- and callable-name-neutral; metadata merely supplies the formal
/// names carrying the policy.
pub(crate) fn apply_only_input_type_bindings(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    only_input_formals: &[String],
    explicit_type_arguments: impl ExplicitTypeArgumentFixity,
    actual_receiver: Option<Ty>,
    actual_arguments: &[(usize, CallArgKind, bool)],
    vararg_index: Option<usize>,
    expected_result: Option<Ty>,
    bindings: &mut GSigBinds,
) -> bool {
    if only_input_formals.is_empty() {
        return true;
    }

    let mut constrained = Vec::<(String, Vec<Ty>)>::new();
    for formal in only_input_formals {
        let Some(formal_index) = signature
            .formals
            .iter()
            .position(|declared| declared == formal)
        else {
            continue;
        };
        if explicit_type_arguments.fixes(formal_index) {
            continue;
        }

        let mut candidates = Vec::new();
        let mut collect = |shape: Ty, actual: Ty| {
            let mut one_input = GSigBinds::new();
            unify_ty_from_symbols(source, shape, actual, &mut one_input);
            let Some(candidate) = one_input.get(formal).copied() else {
                return;
            };
            // Error/pending and null-only types are not proper inference solutions. In particular,
            // a bare `null` cannot manufacture `Nothing?` as an `@OnlyInputTypes` witness; a nullable
            // receiver or another argument can still contribute a real nullable candidate.
            if candidate.mentions_error()
                || candidate.mentions_pending()
                || null_only(candidate)
                || candidate == Ty::Null
            {
                return;
            }
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        };
        if let (Some(shape), Some(actual)) = (signature.receiver, actual_receiver) {
            collect(shape, actual);
        }
        for (parameter, argument, whole_array) in actual_arguments {
            let parameter = *parameter;
            let Some(mut shape) = signature.params.get(parameter).copied() else {
                continue;
            };
            if vararg_index == Some(parameter) && !*whole_array {
                shape = shape.array_read_elem().unwrap_or(shape);
            }
            collect(shape, argument.ty());
        }
        if let Some(expected) = expected_result {
            collect(signature.ret, expected);
        }
        if candidates.is_empty() {
            return false;
        }
        constrained.push((formal.clone(), candidates));
    }

    fn inputs_admit(
        source: &dyn SymbolSource,
        signature: &GenericSig,
        receiver: Option<Ty>,
        arguments: &[(usize, CallArgKind, bool)],
        vararg_index: Option<usize>,
        bindings: &GSigBinds,
    ) -> bool {
        if let (Some(declared), Some(actual)) = (signature.receiver, receiver) {
            let declared = ty_subst_keep_unbound(declared, bindings);
            if !resolution_subtype(source, actual, declared) {
                return false;
            }
        }
        arguments.iter().all(|(parameter, argument, whole_array)| {
            let parameter = *parameter;
            let Some(mut declared) = signature.params.get(parameter).copied() else {
                return false;
            };
            if vararg_index == Some(parameter) && !*whole_array {
                declared = declared.array_read_elem().unwrap_or(declared);
            }
            let declared = ty_subst_keep_unbound(declared, bindings);
            let actual = argument.ty();
            actual.mentions_error()
                || argument.adapts_integer_literal_to(declared)
                || argument.binds_unconstrained_result_to(source, declared)
                || resolution_subtype(source, actual, declared)
        })
    }

    fn choose(
        source: &dyn SymbolSource,
        signature: &GenericSig,
        receiver: Option<Ty>,
        arguments: &[(usize, CallArgKind, bool)],
        vararg_index: Option<usize>,
        constrained: &[(String, Vec<Ty>)],
        index: usize,
        trial: &mut GSigBinds,
    ) -> bool {
        if index == constrained.len() {
            return inputs_admit(source, signature, receiver, arguments, vararg_index, trial)
                && generic_bindings_satisfy_bounds(signature, trial, |actual, bound| {
                    resolution_subtype(source, actual, bound)
                });
        }
        let (formal, candidates) = &constrained[index];
        let previous = trial.remove(formal);
        for &candidate in candidates {
            trial.insert(formal.clone(), candidate);
            if choose(
                source,
                signature,
                receiver,
                arguments,
                vararg_index,
                constrained,
                index + 1,
                trial,
            ) {
                return true;
            }
        }
        trial.remove(formal);
        if let Some(previous) = previous {
            trial.insert(formal.clone(), previous);
        }
        false
    }

    let mut trial = bindings.clone();
    if !choose(
        source,
        signature,
        actual_receiver,
        actual_arguments,
        vararg_index,
        &constrained,
        0,
        &mut trial,
    ) {
        return false;
    }
    *bindings = trial;
    true
}

#[derive(Clone, Copy)]
enum ConstraintPosition {
    Lower,
    Upper,
}

impl ConstraintPosition {
    fn through(self, variance: crate::types::TypeVariance) -> Self {
        match variance {
            crate::types::TypeVariance::In => match self {
                Self::Lower => Self::Upper,
                Self::Upper => Self::Lower,
            },
            crate::types::TypeVariance::Invariant | crate::types::TypeVariance::Out => self,
        }
    }
}

#[derive(Default)]
struct CallInferenceConstraints {
    lower: GSigBinds,
    lower_inputs: std::collections::HashMap<String, Vec<(Ty, usize)>>,
    upper: std::collections::HashMap<String, Vec<Ty>>,
}

pub(crate) struct InferredCallBindings {
    pub bindings: GSigBinds,
    /// Every lower constraint before it is approximated to one denotable binding. Callers that
    /// model Kotlin's non-denotable intersection result consume these only after candidate
    /// selection; ordinary applicability continues to use `bindings`.
    pub lower_inputs: std::collections::HashMap<String, Vec<Ty>>,
    pub upper_only: std::collections::HashSet<String>,
    pub upper_bounds: std::collections::HashMap<String, Vec<Ty>>,
    pub bound_violation: Option<GenericBoundViolation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GenericBoundViolation {
    /// Ordinal in the mapped actual-argument stream supplied to constraint collection.
    pub argument: usize,
    pub expected: Ty,
    pub actual: Ty,
}

impl InferredCallBindings {
    /// Concrete solutions supplied solely by contravariant/input positions. A solution is
    /// publishable only when one observed upper is below every other observed upper; otherwise the
    /// constraint remains open and its caller may apply the language's bottom/default policy.
    pub(crate) fn tightest_upper_bindings(&self, source: &dyn SymbolSource) -> GSigBinds {
        self.upper_only
            .iter()
            .filter_map(|formal| {
                let upper_bounds = self.upper_bounds.get(formal)?;
                let selected = upper_bounds.iter().copied().find(|candidate| {
                    upper_bounds
                        .iter()
                        .all(|upper| resolution_subtype(source, *candidate, *upper))
                })?;
                Some((formal.clone(), selected))
            })
            .collect()
    }
}

impl CallInferenceConstraints {
    fn insert(
        &mut self,
        source: &dyn SymbolSource,
        name: &str,
        actual: Ty,
        position: ConstraintPosition,
        argument: usize,
    ) {
        if actual.mentions_error() {
            return;
        }
        let actual = inference_actual(actual);
        if matches!(actual, Ty::TyParam(actual_name, _) if actual_name == name) {
            return;
        }
        match position {
            ConstraintPosition::Lower => {
                self.lower_inputs
                    .entry(name.to_string())
                    .or_default()
                    .push((actual, argument));
                // A formal's FIRST lower constraint keeps a use-site projection intact (`B<*>`
                // against `B<T>` binds `T := out Any?` — the stand-in for kotlinc's captured type):
                // the parameter then substitutes back to the argument's own type, so the call stays
                // applicable, while the VALUE read out of it is approximated where it is recorded.
                // A second, concrete constraint merges as before, which strips the projection and
                // widens — an invariant occurrence then rejects the write, exactly as kotlinc does.
                let merged = match self.lower.get(name).copied() {
                    None => actual,
                    Some(current) => merge_inferred_ty_from_symbols(Some(source), current, actual),
                };
                self.lower.insert(name.to_string(), merged);
            }
            ConstraintPosition::Upper => {
                self.upper.entry(name.to_string()).or_default().push(actual)
            }
        }
    }

    fn solve(mut self, source: &dyn SymbolSource, signature: &GenericSig) -> InferredCallBindings {
        let mut bound_violation = None;
        // Lower bounds and the declaration's upper bounds are one constraint system. A raw join may
        // be wider than the declared bound (`Int` + `Double` joins to `Any` in the nominal fallback,
        // while `<T : Number>` has the valid and more precise solution `Number`). Retain every lower
        // input until this point and select a declared bound only when it admits them all and the
        // complete declared-bound graph accepts that substitution.
        for (index, formal) in signature.formals.iter().enumerate() {
            let Some(current) = self.lower.get(formal).copied() else {
                continue;
            };
            let bounds = signature
                .formal_bounds
                .get(index)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if bounds.is_empty()
                || bounds.iter().all(|bound| {
                    resolution_subtype(source, current, ty_subst_keep_unbound(*bound, &self.lower))
                })
            {
                continue;
            }
            let lowers = self
                .lower_inputs
                .get(formal)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut candidates = bounds
                .iter()
                .map(|bound| ty_subst_keep_unbound(*bound, &self.lower))
                .filter(|candidate| !candidate.mentions_ty_param())
                .filter(|candidate| {
                    lowers
                        .iter()
                        .all(|(actual, _)| resolution_subtype(source, *actual, *candidate))
                })
                .filter(|candidate| {
                    let mut trial = self.lower.clone();
                    trial.insert(formal.clone(), *candidate);
                    bounds.iter().all(|bound| {
                        resolution_subtype(
                            source,
                            *candidate,
                            ty_subst_keep_unbound(*bound, &trial),
                        )
                    })
                })
                .collect::<Vec<_>>();
            candidates.dedup();
            if let [candidate] = candidates.as_slice() {
                self.lower.insert(formal.clone(), *candidate);
                continue;
            }
            let mut prior = None;
            for &(actual, argument) in lowers {
                let trial = prior
                    .map(|known| merge_inferred_ty_from_symbols(Some(source), known, actual))
                    .unwrap_or(actual);
                let mut trial_bindings = self.lower.clone();
                trial_bindings.insert(formal.clone(), trial);
                let within_bounds = bounds.iter().all(|bound| {
                    resolution_subtype(
                        source,
                        actual,
                        ty_subst_keep_unbound(*bound, &trial_bindings),
                    )
                });
                if !within_bounds {
                    let expected = prior.unwrap_or_else(|| {
                        bounds
                            .first()
                            .copied()
                            .map(|bound| ty_subst_keep_unbound(bound, &trial_bindings))
                            .unwrap_or(current)
                    });
                    let violation = GenericBoundViolation {
                        argument,
                        expected,
                        actual,
                    };
                    if bound_violation
                        .is_none_or(|known: GenericBoundViolation| argument < known.argument)
                    {
                        bound_violation = Some(violation);
                    }
                    break;
                }
                prior = Some(trial);
            }
        }
        let upper_only = self
            .upper
            .iter()
            .filter_map(|(formal, upper)| {
                (!self.lower.contains_key(formal)
                    && upper.iter().any(|constraint| *constraint != Ty::Error))
                .then_some(formal.clone())
            })
            .collect();
        InferredCallBindings {
            bindings: self.lower,
            lower_inputs: self
                .lower_inputs
                .into_iter()
                .map(|(formal, inputs)| {
                    (
                        formal,
                        inputs.into_iter().map(|(actual, _)| actual).collect(),
                    )
                })
                .collect(),
            upper_only,
            upper_bounds: self.upper,
            bound_violation,
        }
    }
}

fn collect_call_inference_constraints(
    source: &dyn SymbolSource,
    shape: Ty,
    actual: Ty,
    position: ConstraintPosition,
    argument_ordinal: usize,
    constraints: &mut CallInferenceConstraints,
) {
    match shape {
        Ty::TyParam(name, _) => {
            constraints.insert(source, name, actual, position, argument_ordinal)
        }
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => {
            collect_call_inference_constraints(
                source,
                *inner,
                nullable_generic_actual(actual),
                position,
                argument_ordinal,
                constraints,
            );
        }
        Ty::InProjection(inner) => collect_call_inference_constraints(
            source,
            *inner,
            match actual {
                Ty::OutProjection(_) | Ty::StarProjection(_) => Ty::Nothing,
                _ => actual.projection_inner().unwrap_or(actual),
            },
            position.through(crate::types::TypeVariance::In),
            argument_ordinal,
            constraints,
        ),
        Ty::OutProjection(inner) => collect_call_inference_constraints(
            source,
            *inner,
            actual.projection_inner().unwrap_or(actual),
            position,
            argument_ordinal,
            constraints,
        ),
        Ty::StarProjection(_) => {}
        Ty::Fun(function) => {
            let Ty::Fun(actual) = actual.non_null() else {
                return;
            };
            for (&parameter, &actual) in function.params.iter().zip(&actual.params) {
                collect_call_inference_constraints(
                    source,
                    parameter,
                    actual,
                    position.through(crate::types::TypeVariance::In),
                    argument_ordinal,
                    constraints,
                );
            }
            collect_call_inference_constraints(
                source,
                function.ret,
                actual.ret,
                position,
                argument_ordinal,
                constraints,
            );
        }
        Ty::Obj(owner, arguments) => {
            let mut actual = actual.projection_inner().unwrap_or(actual).non_null();
            // A caller-owned type parameter contributes the applied shape of its upper bound to a
            // classifier-shaped constraint. Treating the variable itself as the callee's element
            // argument makes `T : Iterable<*>` bind `Iterable<E>.withIndex` as `E = T`; project
            // through the bound so it correctly binds `E` to the star's readable upper type.
            while let Ty::TyParam(_, bound) = actual {
                actual = bound.projection_inner().unwrap_or(*bound).non_null();
            }
            let projected = match actual {
                Ty::Obj(actual_owner, _) if owner == actual_owner => Some(actual),
                Ty::Obj(_, _) => receiver_hierarchy(source, actual)
                    .into_iter()
                    .map(|(ty, _)| ty)
                    .find(|ty| {
                        ty.obj_internal()
                            .is_some_and(|actual_owner| owner == actual_owner)
                    }),
                _ => None,
            };
            let Some(Ty::Obj(_, actual_arguments)) = projected else {
                return;
            };
            let classifier = source.classifier(owner);
            let variances = classifier
                .as_ref()
                .map(|classifier| classifier.type_param_variances.clone())
                .unwrap_or_default();
            for (index, (&argument, &actual)) in arguments.iter().zip(actual_arguments).enumerate()
            {
                let declared_variance = variances
                    .get(index)
                    .copied()
                    .unwrap_or(crate::types::TypeVariance::Invariant);
                // A matching use-site projection is redundant on a variant declaration. `A<*>`
                // for `A<out T : Any>` exposes the readable upper `Any`; retaining the star as a
                // call-formal binding would later instantiate `A<E>` in input position as
                // `A<Nothing>` and reject the very argument that supplied the capture. Invariant
                // classifiers must keep the projection because their write side really is closed.
                let actual = match (declared_variance, actual) {
                    (crate::types::TypeVariance::Out, Ty::OutProjection(inner))
                    | (crate::types::TypeVariance::Out, Ty::StarProjection(inner))
                    | (crate::types::TypeVariance::In, Ty::InProjection(inner)) => *inner,
                    _ => actual,
                };
                let actual = if matches!(argument, Ty::OutProjection(_))
                    && matches!(actual, Ty::InProjection(_))
                {
                    let upper = classifier
                        .as_ref()
                        .and_then(|classifier| classifier.type_param_bounds.get(index))
                        .and_then(|bounds| bounds.first())
                        .copied()
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                    crate::trace_compiler!(
                        "resolve",
                        "projected inference owner={owner:?} index={index} shape={argument:?} actual={actual:?} readable={upper:?}"
                    );
                    upper
                } else {
                    actual
                };
                // An invariant classifier argument is an equality constraint even when the
                // classifier itself occurs contravariantly. For `(Box<T>) -> Unit` matched against
                // `(Box<Value>) -> Unit`, `T` is `Value`, not an upper-only variable defaulting to
                // `Nothing`. Use-site projections still carry their own direction in `argument`.
                let argument_position = match declared_variance {
                    crate::types::TypeVariance::Invariant => ConstraintPosition::Lower,
                    variance => position.through(variance),
                };
                collect_call_inference_constraints(
                    source,
                    argument,
                    actual,
                    argument_position,
                    argument_ordinal,
                    constraints,
                );
            }
        }
        _ => {}
    }
}

/// Complete a call's INFERRED bindings against `where`-clause subtype constraints, in place. When
/// a formal's bound names another formal (`C : R`) whose inferred binding is a BOTTOM
/// (`ifBlank { null }` binds `R := Nothing?`, `{ error(..) }` binds `R := Nothing`) while the
/// constraining side is not, the binding becomes their JOIN — the constraining binding, made
/// nullable when the bottom admitted null (`R = String?` / `R = String`), which is kotlinc's
/// inference. Runs on the caller's REAL bindings — the return type substitutes from them, so a
/// check-local completion would admit the candidate while leaving the bottom to poison the
/// signature. The leading `explicit` formals were WRITTEN at the call site and are never rewritten
/// (kotlinc rejects an explicit `<String, Nothing?>` against `C : R`, and so does the unchanged
/// bounds check).
pub(crate) fn complete_bottom_constraint_bindings(
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    loop {
        let mut changed = false;
        for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
            let Some(actual) = bindings.get(formal).copied() else {
                continue;
            };
            for bound in bounds {
                let Ty::TyParam(bound_formal, _) = bound.non_null() else {
                    continue;
                };
                let Some(position) = generic_sig
                    .formals
                    .iter()
                    .position(|candidate| candidate == bound_formal)
                else {
                    continue;
                };
                if explicit.fixes(position) {
                    continue;
                }
                let is_bottom = |ty: Ty| ty == Ty::Null || ty.non_null() == Ty::Nothing;
                match bindings.get(bound_formal).copied() {
                    Some(bottom) if is_bottom(bottom) && !is_bottom(actual) => {
                        // `Null` (the raw literal type) and `Nothing?` both admitted null.
                        let joined = if bottom == Ty::Null || bottom.is_nullable() {
                            Ty::nullable(actual)
                        } else {
                            actual
                        };
                        if joined != bottom {
                            bindings.insert(bound_formal.to_string(), joined);
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Materialize a type argument that is observable only through a bound which another inferred
/// formal has made concrete. In `<T : KFunction<E>, E : Any> f(A<E>)`, the argument fixes `E`; `T`
/// has no independent input/result occurrence, so its unique solution is the now-concrete bound
/// `KFunction<E>`. This is different from an unconstrained `<T : Any> f()`: a static bound alone is
/// not call-site evidence and remains unbound.
pub(crate) fn complete_dependency_instantiated_bound_bindings(
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    loop {
        let mut additions = Vec::new();
        for (index, (formal, bounds)) in generic_sig
            .formals
            .iter()
            .zip(&generic_sig.formal_bounds)
            .enumerate()
        {
            if explicit.fixes(index) || bindings.contains_key(formal) {
                continue;
            }
            let observable = generic_sig.receiver.is_some_and(|receiver| {
                crate::types::ty_mentions_param(receiver, std::slice::from_ref(formal))
            }) || generic_sig.params.iter().any(|parameter| {
                crate::types::ty_mentions_param(*parameter, std::slice::from_ref(formal))
            }) || crate::types::ty_mentions_param(
                generic_sig.ret,
                std::slice::from_ref(formal),
            );
            if observable {
                continue;
            }
            let [bound] = bounds.as_slice() else {
                continue;
            };
            let depends_on_inferred_formal = generic_sig.formals.iter().any(|dependency| {
                dependency != formal
                    && bindings.contains_key(dependency)
                    && crate::types::ty_mentions_param(*bound, std::slice::from_ref(dependency))
            });
            if !depends_on_inferred_formal {
                continue;
            }
            let solution = ty_subst_keep_unbound(*bound, bindings).projection_read_ty();
            if solution != Ty::Error
                && !solution.mentions_pending()
                && !solution.mentions_ty_param()
            {
                additions.push((formal.clone(), solution));
            }
        }
        if additions.is_empty() {
            break;
        }
        bindings.extend(additions);
    }
}

/// Complete a method type parameter whose only call-site information is the readable upper bound
/// of a star-captured receiver parameter.
///
/// `Entity<*>.value<T : S>(): T` is not the same as an unconstrained `value<T>(): T`: selecting the
/// member has already supplied the capture for the owner's `S`. Kotlin reads that capture through
/// its upper bound and approximates the recursive, non-denotable part back to a star before the
/// result enters a join or checked FIR. A source star retains that identity while carrying its
/// readable bound, so recursively approximating `S` produces a denotable `Entity<*>` rather than
/// inventing an explicit `out Any?` projection.
///
/// This deliberately applies only to a return-only method formal with one direct owner-formal bound.
/// Ordinary `<T : Any> value(): T`, input-constrained formals, and multi-bound intersections remain
/// unbound until the normal constraint system supplies evidence.
pub(crate) fn complete_return_only_captured_receiver_bindings(
    generic_sig: &GenericSig,
    receiver_bindings: &GSigBinds,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    for (index, (formal, bounds)) in generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .enumerate()
    {
        if explicit.fixes(index)
            || bindings.contains_key(formal)
            || !crate::types::ty_mentions_param(generic_sig.ret, std::slice::from_ref(formal))
            || generic_sig.params.iter().any(|parameter| {
                crate::types::ty_mentions_param(*parameter, std::slice::from_ref(formal))
            })
        {
            continue;
        }
        let [bound] = bounds.as_slice() else {
            continue;
        };
        let star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
        let mut recursive_approximation = GSigBinds::new();
        for (receiver_formal, captured) in receiver_bindings {
            if captured.projection_inner().is_some()
                && crate::types::ty_mentions_param(*bound, std::slice::from_ref(receiver_formal))
            {
                recursive_approximation.insert(receiver_formal.clone(), star);
            }
        }
        let captured_upper = match bound.non_null() {
            Ty::TyParam(receiver_formal, _) => receiver_bindings
                .get(receiver_formal)
                .copied()
                .filter(|captured| captured.projection_inner().is_some())
                .map(Ty::projection_read_ty),
            projected @ (Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_)) => {
                Some(projected.projection_read_ty())
            }
            _ if !recursive_approximation.is_empty() => Some(*bound),
            _ => None,
        };
        let Some(captured_upper) = captured_upper else {
            continue;
        };
        let solution =
            ty_subst_keep_unbound(captured_upper, &recursive_approximation).projection_read_ty();
        let retains_method_formal = generic_sig
            .formals
            .iter()
            .any(|formal| crate::types::ty_mentions_param(solution, std::slice::from_ref(formal)));
        // A class or caller type parameter is a denotable part of the selected result and may cross
        // into checked FIR (`Entity<U, *>`). Only a still-unresolved formal owned by this method
        // would make the completion circular. The old blanket `mentions_ty_param` test rejected the
        // valid caller-owned `U` together with the captured recursive `S` that was already
        // approximated above.
        if solution != Ty::Error && !solution.mentions_pending() && !retains_method_formal {
            bindings.insert(formal.clone(), solution);
        }
    }
}

/// Materialize type arguments implied solely by a bound between declaration formals. Applicability
/// has always used this relation (`<T1 : C, T2 : T1>` plus `T2 = C` entails `T1 = C`); completing
/// the caller-owned map here ensures the selected call publishes the same solution to FIR.
fn complete_dependent_bound_bindings(
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    loop {
        let mut changed = false;
        for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
            let Some(actual) = bindings.get(formal).copied() else {
                continue;
            };
            for bound in bounds {
                let Ty::TyParam(bound_formal, _) = bound.non_null() else {
                    continue;
                };
                let Some(bound_index) = generic_sig
                    .formals
                    .iter()
                    .position(|candidate| candidate == bound_formal)
                else {
                    continue;
                };
                if !explicit.fixes(bound_index) && !bindings.contains_key(bound_formal) {
                    bindings.insert(bound_formal.to_string(), actual);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Complete a written underscore from its one concrete upper bound after stronger constraints have
/// run. An omitted type-argument list is deliberately unaffected.
fn complete_explicit_hole_bound_bindings(
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    loop {
        let mut additions = Vec::new();
        for (index, (formal, bounds)) in generic_sig
            .formals
            .iter()
            .zip(&generic_sig.formal_bounds)
            .enumerate()
        {
            if !explicit.inferred_hole(index) || bindings.contains_key(formal) {
                continue;
            }
            let [bound] = bounds.as_slice() else {
                continue;
            };
            let solution = ty_subst_keep_unbound(*bound, bindings);
            if solution != Ty::Error && !solution.mentions_ty_param() {
                additions.push((formal.clone(), solution));
            }
        }
        if additions.is_empty() {
            break;
        }
        bindings.extend(additions);
    }
}

/// Solve a formal from the declaration's own bound relation: one that no argument reached, and one
/// whose argument-derived binding its OWN bound forbids.
///
/// An argument can pin a type variable to something its declared bound rules out. For
/// `fun <T : Base<T>, C : T> f(self: C, subs: Iterable<T>)` called as `f(Auth(), listOf(Login()))`,
/// the element type pins `T = Login` — but `Login` is a `Base<Cmd>`, not a `Base<Login>`, so that
/// binding cannot be what the call means. Kotlin solves `T` from the same recursive bound: the
/// application of `Base` in `Login`'s hierarchy is `Base<Cmd>`, so `T = Cmd`. Keeping the violating
/// binding instead makes the parameter `Iterable<Login>` and the call is reported inapplicable.
///
/// A concrete binding may also expose another formal through an applied generic supertype:
/// `<P, C : Component<P, *>>` with `C = MyComponent<X>` entails `P = MyProps<X>` when that is the
/// `Component` supertype of `MyComponent<X>`. Only unbound, non-explicit formals are filled through
/// this relation. A caller with no symbol source — or a bound the walk cannot reach — leaves every
/// binding alone.
pub(crate) fn resolve_bound_violating_bindings(
    source: Option<&dyn SymbolSource>,
    generic_sig: &GenericSig,
    bindings: &mut GSigBinds,
    explicit: impl ExplicitTypeArgumentFixity,
) {
    let Some(source) = source else {
        complete_explicit_hole_bound_bindings(generic_sig, bindings, explicit);
        return;
    };
    let oracle = SourceOracle(source);
    // A formal mentioned only as another formal's upper bound is the join of every concrete
    // subtype that reaches it. `T : V, U : V` with both `T` and `U` inferred must therefore widen
    // the dependency-only `V`; the first completed edge is not an equality that may freeze it.
    // A formal occurring in a value parameter has independent call-site evidence and is left to
    // the ordinary argument solver.
    for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
        let Some(actual) = bindings.get(formal).copied() else {
            continue;
        };
        for bound in bounds {
            let Ty::TyParam(bound_formal, _) = bound.non_null() else {
                continue;
            };
            let Some(bound_index) = generic_sig
                .formals
                .iter()
                .position(|candidate| candidate == bound_formal)
            else {
                continue;
            };
            let bound_name = bound_formal.to_string();
            let directly_constrained = generic_sig.params.iter().any(|parameter| {
                crate::types::ty_mentions_param(*parameter, std::slice::from_ref(&bound_name))
            });
            if explicit.fixes(bound_index) || directly_constrained || actual.mentions_ty_param() {
                continue;
            }
            let merged = bindings.get(bound_formal).copied().map_or(actual, |known| {
                merge_inferred_ty_from_symbols(Some(source), known, actual)
            });
            bindings.insert(bound_formal.to_string(), merged);
        }
    }
    // Apply each concrete formal binding to its declaration bounds, then unify the actual applied
    // supertype with the still-symbolic bound. This is ordinary constraint propagation through the
    // class graph, not subtype guessing: the provider supplies the exact applied supertype and the
    // normal unifier extracts only declaration-owned variables from matching positions.
    loop {
        let mut additions = GSigBinds::new();
        for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
            let Some(actual) = bindings.get(formal).copied() else {
                continue;
            };
            for bound in bounds {
                let Some(target) = bound.kotlin_class_internal() else {
                    continue;
                };
                let Some(applied) =
                    crate::assignable::applied_supertype(&oracle, actual, Ty::obj_name(target))
                else {
                    continue;
                };
                let mut inferred = GSigBinds::new();
                unify_ty_from_symbols(source, *bound, applied, &mut inferred);
                for (candidate, solution) in inferred {
                    let Some(index) = generic_sig
                        .formals
                        .iter()
                        .position(|declared| declared == &candidate)
                    else {
                        continue;
                    };
                    if candidate == *formal
                        || explicit.fixes(index)
                        || bindings.contains_key(&candidate)
                        || solution == Ty::Error
                        || solution.mentions_ty_param()
                    {
                        continue;
                    }
                    additions
                        .entry(candidate)
                        .and_modify(|known| {
                            *known = merge_inferred_ty(Some(*known), solution);
                        })
                        .or_insert(solution);
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        bindings.extend(additions);
    }
    // A written `_` is an explicit request to infer this position. After argument and applied-bound
    // propagation have had priority, a single concrete declared upper bound is the remaining
    // solution Kotlin publishes (`<P, C : Component<P, *>>` with explicit `P` and `C = _`). Do not
    // apply this to an omitted type-argument list: a wholly unconstrained ordinary call still
    // receives its normal not-enough-information treatment.
    complete_explicit_hole_bound_bindings(generic_sig, bindings, explicit);
    // A formal can be reachable ONLY through another's bound: `<T : Base<T>, C : T>` mentions `T` in
    // no parameter position of `f(self: C, subs: Array<out T>)` once the argument is a vararg
    // element. `C`'s value answers it through the same recursive bound.
    for (index, bounds) in generic_sig.formal_bounds.iter().enumerate() {
        if explicit.fixes(index) {
            continue;
        }
        let Some(actual) = generic_sig
            .formals
            .get(index)
            .and_then(|formal| bindings.get(formal))
            .copied()
        else {
            continue;
        };
        for bound in bounds {
            let Ty::TyParam(open, _) = bound.non_null() else {
                continue;
            };
            let Some(open_index) = generic_sig
                .formals
                .iter()
                .position(|candidate| candidate == open)
            else {
                continue;
            };
            if explicit.fixes(open_index) || bindings.contains_key(open) {
                continue;
            }
            let Some(open_bounds) = generic_sig.formal_bounds.get(open_index) else {
                continue;
            };
            for open_bound in open_bounds {
                let Some(position) = open_bound.type_args().iter().position(
                    |argument| matches!(argument.non_null(), Ty::TyParam(name, _) if name == open),
                ) else {
                    continue;
                };
                let Some(target) = open_bound.kotlin_class_internal() else {
                    continue;
                };
                let Some(applied) =
                    crate::assignable::applied_supertype(&oracle, actual, Ty::obj_name(target))
                else {
                    continue;
                };
                let Some(&solution) = applied.type_args().get(position) else {
                    continue;
                };
                if solution != Ty::Error && !solution.mentions_ty_param() {
                    bindings.insert(open.to_string(), solution);
                    break;
                }
            }
        }
    }
    for (index, (formal, bounds)) in generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .enumerate()
    {
        if explicit.fixes(index) {
            continue;
        }
        let Some(actual) = bindings.get(formal).copied() else {
            continue;
        };
        for bound in bounds {
            // Only a bound that mentions the formal ITSELF can be re-solved this way: it is what
            // ties the variable to a position in its own hierarchy.
            let Some(position) = bound.type_args().iter().position(
                |argument| matches!(argument.non_null(), Ty::TyParam(name, _) if name == formal),
            ) else {
                continue;
            };
            let applied_bound = ty_subst_keep_unbound(*bound, bindings);
            if crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &oracle,
                actual,
                applied_bound,
            ) {
                continue;
            }
            let Some(target) = bound.kotlin_class_internal() else {
                continue;
            };
            let Some(applied) =
                crate::assignable::applied_supertype(&oracle, actual, Ty::obj_name(target))
            else {
                continue;
            };
            let Some(&solution) = applied.type_args().get(position) else {
                continue;
            };
            if solution != Ty::Error && !solution.mentions_ty_param() && solution != actual {
                bindings.insert(formal.clone(), solution);
                break;
            }
        }
    }
}

pub(crate) fn generic_bindings_satisfy_bounds(
    generic_sig: &GenericSig,
    bindings: &GSigBinds,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    // A bound can constrain another formal: `<T : X, X : Comparable<UInt>>`. When an argument binds
    // `T` but no argument mentions `X`, Kotlin infers the most specific `X` from that subtype
    // constraint. Complete those relationships before substituting/checking the bound graph; leaving
    // `X` open erases it to its first bound and incorrectly asks whether `UInt` is the raw
    // `Comparable` parameter type at the call site.
    let mut bindings = bindings.clone();
    complete_dependent_bound_bindings(generic_sig, &mut bindings, 0);
    generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .all(|(formal, bounds)| {
            let Some(actual) = bindings.get(formal).copied() else {
                return true;
            };
            let actual = actual.projection_inner().unwrap_or(actual);
            bounds
                .iter()
                .all(|bound| admits(actual, ty_subst(*bound, &bindings)))
        })
}

/// Validate final call bindings while preserving Kotlin's expected-result intersection rule.
/// Every ordinary bound must hold. The sole exception is a conflicting binding contributed by the
/// expected result for a return-only formal; that binding denotes `expected & declared bounds`, not
/// an unconstrained replacement of the declared bound.
pub(crate) fn generic_bindings_admit_expected_return_intersection(
    generic_sig: &GenericSig,
    bindings: &GSigBinds,
    expected_bindings: Option<&GSigBinds>,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .all(|(formal, bounds)| {
            let Some(actual) = bindings.get(formal).copied() else {
                return true;
            };
            let actual = actual.projection_inner().unwrap_or(actual);
            bounds.iter().all(|bound| {
                let bound = ty_subst(*bound, bindings);
                if admits(actual, bound) {
                    return true;
                }
                let formal_slice = std::slice::from_ref(formal);
                expected_bindings
                    .and_then(|expected| expected.get(formal))
                    .is_some_and(|expected| expected.non_null() == actual.non_null())
                    && crate::types::ty_mentions_param(generic_sig.ret, formal_slice)
                    && generic_sig.receiver.is_none_or(|receiver| {
                        !crate::types::ty_mentions_param(receiver, formal_slice)
                    })
                    && generic_sig
                        .params
                        .iter()
                        .all(|parameter| !crate::types::ty_mentions_param(*parameter, formal_slice))
                    && actual.is_reference()
                    && bound.is_reference()
            })
        })
}

/// Infer method type arguments from an expected result and validate their declared bounds.
/// Expected types are upper constraints on the produced value: when `T : Any` flows into `String?`,
/// `T = String` is the valid solution (`String` is assignable to `String?`), while binding the nullable
/// destination verbatim would incorrectly violate the non-null bound.
pub(crate) fn infer_generic_return_bindings(
    generic_sig: &GenericSig,
    expected: Ty,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> Option<GSigBinds> {
    let mut bindings = GSigBinds::new();
    unify_inferred_ty(generic_sig.ret, expected, &mut bindings);
    if bindings.is_empty() {
        return None;
    }
    for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
        let Some(actual) = bindings.get(formal).copied() else {
            continue;
        };
        if !actual.is_nullable() || bounds.is_empty() {
            continue;
        }
        let narrowed = actual.non_null();
        let actual_satisfies = bounds
            .iter()
            .all(|bound| admits(actual, ty_subst(*bound, &bindings)));
        let narrowed_satisfies = bounds
            .iter()
            .all(|bound| admits(narrowed, ty_subst(*bound, &bindings)));
        if !actual_satisfies && narrowed_satisfies {
            bindings.insert(formal.clone(), narrowed);
        }
    }
    // An expected result is an upper constraint on a produced value. If a return-only variable's
    // declared bound is already a subtype of that expectation, their conceptual intersection is
    // exactly the bound (`<T : I2> make(): T` used where `I` is expected, with `I2 : I`, produces
    // `I2`). Keeping the broader expected type as an equality both violates T's bound and discards
    // the precise flow type needed by the consumer. Incomparable reference constituents remain on
    // the non-denotable intersection path below.
    for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
        let Some(mut actual) = bindings.get(formal).copied() else {
            continue;
        };
        let formal_slice = std::slice::from_ref(formal);
        if !crate::types::ty_mentions_param(generic_sig.ret, formal_slice)
            || generic_sig
                .receiver
                .is_some_and(|receiver| crate::types::ty_mentions_param(receiver, formal_slice))
            || generic_sig
                .params
                .iter()
                .any(|parameter| crate::types::ty_mentions_param(*parameter, formal_slice))
        {
            continue;
        }
        for bound in bounds {
            let bound = ty_subst_keep_unbound(*bound, &bindings);
            if admits(bound, actual) {
                actual = bound;
            }
        }
        bindings.insert(formal.clone(), actual);
    }
    if generic_bindings_satisfy_bounds(generic_sig, &bindings, |actual, bound| {
        admits(actual, bound)
    }) {
        return Some(bindings);
    }

    // An expected result may constrain a declaration-owned variable to an intersection with its
    // upper bound: `<T : FinalClass> make(): T?` used as `SomeInterface?` has the conceptual result
    // `(FinalClass & SomeInterface)?`. Kotlin admits the call (and reports an empty-intersection
    // warning when the constituents provably cannot meet). `Ty` intentionally carries denotable
    // use-site types, so retain the expected constituent as the checked result projection. This is
    // valid only for a RETURN-ONLY formal: a receiver or value argument would be concrete input
    // evidence and its declared bounds remain mandatory applicability constraints.
    let mut has_intersection = false;
    for (formal, bounds) in generic_sig.formals.iter().zip(&generic_sig.formal_bounds) {
        let Some(mut actual) = bindings.get(formal).copied() else {
            continue;
        };
        let formal_slice = std::slice::from_ref(formal);
        if !crate::types::ty_mentions_param(generic_sig.ret, formal_slice)
            || generic_sig
                .receiver
                .is_some_and(|receiver| crate::types::ty_mentions_param(receiver, formal_slice))
            || generic_sig
                .params
                .iter()
                .any(|parameter| crate::types::ty_mentions_param(*parameter, formal_slice))
        {
            return None;
        }
        if actual.is_nullable() && bounds.iter().all(|bound| !bound.admits_null()) {
            actual = actual.non_null();
            bindings.insert(formal.clone(), actual);
        }
        if !actual.is_reference() {
            return None;
        }
        for bound in bounds {
            let bound = ty_subst(*bound, &bindings);
            if !bound.is_reference() {
                return None;
            }
            has_intersection |= !admits(actual, bound);
        }
    }
    has_intersection.then_some(bindings)
}

/// Infer a generic result against an expected supertype. Generic return inference normally compares
/// equal constructors directly; a nested call also needs the ordinary applied-supertype relation
/// (`MutableSet<T>` used where `MutableCollection<in R>` is expected). Providers publish only direct
/// templates, so core performs the same structural hierarchy walk used by assignability.
pub(crate) fn infer_generic_return_bindings_from_symbols(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    expected: Ty,
    admits: impl FnMut(Ty, Ty) -> bool,
) -> Option<GSigBinds> {
    let declared = return_shape_at_expected_owner(source, generic_sig.ret, expected)?;
    let mut projected = generic_sig.clone();
    projected.ret = declared;
    infer_generic_return_bindings(&projected, expected, admits)
}

/// The declared return restated at the expected type's own constructor. `MutableReply<T>` used where
/// `Reply<Any>` is expected relates through the applied supertype `Reply<T>`; equal constructors are
/// already comparable, and an unrelated constructor has no relation to report.
fn return_shape_at_expected_owner(
    source: &dyn SymbolSource,
    declared: Ty,
    expected: Ty,
) -> Option<Ty> {
    match (declared.non_null(), expected.non_null()) {
        (Ty::Obj(declared_owner, _), Ty::Obj(expected_owner, _))
            if declared_owner != expected_owner =>
        {
            receiver_hierarchy(source, declared.non_null())
                .into_iter()
                .map(|(ty, _)| ty)
                .find(|ty| {
                    ty.obj_internal()
                        .is_some_and(|owner| owner == expected_owner)
                })
        }
        _ => Some(declared),
    }
}

/// Widen argument-derived bindings that an expected result fixes invariantly. Value arguments only
/// contribute lower bounds — `reply("s")` infers `T = String` — but an invariant occurrence of the
/// formal in the declared return admits exactly one solution, so `Reply<Any>` in return position
/// forces `T = Any` the way kotlinc's constraint solver does. Covariant occurrences keep the narrower
/// argument solution (`listOf("x")` stays `List<String>` where `List<Any>` is expected), a projected
/// expectation (`Reply<out Any>`) is a bound rather than an equality, and a widening the argument
/// itself cannot satisfy — or that breaks a declared bound — is left alone so the call is still
/// reported against its real type.
pub(crate) fn widen_invariant_expected_bindings(
    source: &dyn SymbolSource,
    signature: &GenericSig,
    explicit_type_arguments: impl ExplicitTypeArgumentFixity,
    bindings: &mut GSigBinds,
    expected_bindings: &GSigBinds,
    expected: Ty,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) {
    let Some(declared) = return_shape_at_expected_owner(source, signature.ret, expected) else {
        return;
    };
    // Declaration order, not map order: a widening is accepted against the bindings the earlier
    // formals already settled, so the outcome must not depend on iteration order.
    for (index, formal) in signature.formals.iter().enumerate() {
        if explicit_type_arguments.fixes(index) {
            continue;
        }
        let Some((&expected_argument, &inferred)) =
            expected_bindings.get(formal).zip(bindings.get(formal))
        else {
            continue;
        };
        if !formal_has_unprojected_expected_occurrence(declared, expected, formal) {
            continue;
        }
        if inferred == expected_argument
            || formal_variance_in_type(source, declared, formal)
                != Some(crate::types::TypeVariance::Invariant)
            || !admits(inferred, expected_argument)
        {
            continue;
        }
        let mut trial = bindings.clone();
        trial.insert(formal.clone(), expected_argument);
        if generic_bindings_satisfy_bounds(signature, &trial, &mut admits) {
            *bindings = trial;
        }
    }
}

/// Symbolic constraints contributed by an expected return type. `constrained_formals` also contains
/// formals whose occurrences disagreed, so ordinary inference cannot replace a real conflict with a
/// last-write-wins binding.
pub(crate) struct SymbolicReturnConstraints {
    pub bindings: GSigBinds,
    pub constrained_formals: std::collections::HashSet<String>,
    pub conflicting_formals: std::collections::HashSet<String>,
}

/// Relate callee-owned return variables to caller-owned symbolic expected types. The two scopes are
/// deliberately independent: `<T> build(): List<T>` used from `<U> outer(): List<U>` constrains the
/// callee's `T` to the caller's `U`; equal source spelling is neither required nor treated as identity.
/// Only matching type constructors are traversed, and repeated occurrences must agree exactly.
pub(crate) fn infer_generic_symbolic_return_constraints(
    declared: Ty,
    expected: Ty,
    formals: &[String],
) -> SymbolicReturnConstraints {
    fn symbolic_parameter(ty: Ty) -> bool {
        match ty {
            Ty::TyParam(..) => true,
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => symbolic_parameter(*inner),
            _ => false,
        }
    }

    fn collect(
        declared: Ty,
        expected: Ty,
        formals: &[String],
        candidates: &mut std::collections::HashMap<String, (Option<Ty>, bool)>,
        constrained_formals: &mut std::collections::HashSet<String>,
    ) {
        match (declared, expected) {
            (Ty::TyParam(declared, _), expected_ty)
                if formals.iter().any(|formal| formal.as_str() == declared) =>
            {
                let symbolic = symbolic_parameter(expected_ty);
                if symbolic {
                    constrained_formals.insert(declared.to_string());
                }
                match candidates.entry(declared.to_string()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert((Some(expected_ty), symbolic));
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        let (current, all_symbolic) = entry.get_mut();
                        *all_symbolic &= symbolic;
                        if current.is_some_and(|current| current != expected_ty) {
                            *current = None;
                        }
                    }
                }
            }
            (Ty::Obj(declared_name, declared_args), Ty::Obj(expected_name, expected_args))
                if declared_name == expected_name && declared_args.len() == expected_args.len() =>
            {
                for (&declared, &expected) in declared_args.iter().zip(expected_args.iter()) {
                    collect(declared, expected, formals, candidates, constrained_formals);
                }
            }
            (Ty::Fun(declared), Ty::Fun(expected))
                if declared.params.len() == expected.params.len()
                    && declared.context_count == expected.context_count
                    && declared.has_receiver == expected.has_receiver
                    && declared.suspend == expected.suspend =>
            {
                for (&declared, &expected) in declared.params.iter().zip(expected.params.iter()) {
                    collect(declared, expected, formals, candidates, constrained_formals);
                }
                collect(
                    declared.ret,
                    expected.ret,
                    formals,
                    candidates,
                    constrained_formals,
                );
            }
            (Ty::Nullable(declared), Ty::Nullable(expected))
            | (Ty::PlatformNullable(declared), Ty::PlatformNullable(expected))
            | (Ty::InProjection(declared), Ty::InProjection(expected))
            | (Ty::OutProjection(declared), Ty::OutProjection(expected)) => {
                collect(
                    *declared,
                    *expected,
                    formals,
                    candidates,
                    constrained_formals,
                );
            }
            (Ty::StarProjection(_), Ty::StarProjection(_)) => {}
            _ => {}
        }
    }

    let mut candidates = std::collections::HashMap::new();
    let mut constrained_formals = std::collections::HashSet::new();
    collect(
        declared,
        expected,
        formals,
        &mut candidates,
        &mut constrained_formals,
    );
    let mut bindings = GSigBinds::new();
    let mut conflicting_formals = std::collections::HashSet::new();
    for (formal, (actual, all_symbolic)) in candidates {
        if let Some(actual) = actual.filter(|_| all_symbolic) {
            bindings.insert(formal, actual);
        } else if actual.is_none() {
            conflicting_formals.insert(formal);
        }
    }
    SymbolicReturnConstraints {
        bindings,
        constrained_formals,
        conflicting_formals,
    }
}

/// A JVM method signature may reference owner type parameters without declaring them. Recover those
/// bindings from the provider's receiver-specialized return; method-owned formals still bind from args.
pub(super) fn seed_undeclared_return_bindings(
    sig: Ty,
    actual: Ty,
    declared_formals: &[String],
    binds: &mut GSigBinds,
) {
    match sig {
        Ty::TyParam(n, _)
            if !declared_formals.iter().any(|formal| formal == n) && actual != Ty::Error =>
        {
            binds.entry(n.to_string()).or_insert(actual);
        }
        Ty::Fun(fsig) => {
            if let Ty::Fun(afsig) = actual {
                for (s, a) in fsig.params.iter().zip(afsig.params.iter()) {
                    seed_undeclared_return_bindings(*s, *a, declared_formals, binds);
                }
                seed_undeclared_return_bindings(fsig.ret, afsig.ret, declared_formals, binds);
            }
        }
        Ty::Nullable(inner) => {
            if let Ty::Nullable(actual_inner) = actual {
                seed_undeclared_return_bindings(*inner, *actual_inner, declared_formals, binds);
            } else {
                seed_undeclared_return_bindings(*inner, actual, declared_formals, binds);
            }
        }
        Ty::Obj(_, args) => {
            if let Ty::Obj(_, actual_args) = actual {
                for (s, a) in args.iter().zip(actual_args.iter()) {
                    seed_undeclared_return_bindings(*s, *a, declared_formals, binds);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn merge_specialized_return(provider: Ty, inferred: Ty) -> Ty {
    if provider == Ty::Error {
        return inferred;
    }
    // A provider return can still contain the declaration's unbound formal while call inference has
    // already specialized the same position. The inferred concrete type is authoritative here:
    // merging `List<R>` with `List<Int>` must produce `List<Int>`, not restore `R` and discard the
    // callable argument's return constraint.
    if matches!(provider.non_null(), Ty::TyParam(_, _)) && !inferred.mentions_ty_param() {
        return inferred;
    }
    if inferred == Ty::Error
        || (inferred.is_erased_top() && !matches!(inferred.non_null(), Ty::TyParam(_, _)))
    {
        return provider;
    }
    if provider.is_erased_top() {
        return inferred;
    }
    match (provider, inferred) {
        (Ty::PlatformNullable(provider), Ty::PlatformNullable(inferred)) => {
            Ty::platform_nullable(merge_specialized_return(*provider, *inferred))
        }
        (Ty::PlatformNullable(provider), inferred) => {
            Ty::platform_nullable(merge_specialized_return(*provider, inferred.non_null()))
        }
        (provider, Ty::PlatformNullable(inferred)) => {
            Ty::platform_nullable(merge_specialized_return(provider.non_null(), *inferred))
        }
        (Ty::Nullable(provider), Ty::Nullable(inferred)) => {
            Ty::nullable(merge_specialized_return(*provider, *inferred))
        }
        (Ty::Nullable(provider), inferred) => {
            Ty::nullable(merge_specialized_return(*provider, inferred))
        }
        (provider, Ty::Nullable(inferred)) => {
            Ty::nullable(merge_specialized_return(provider, *inferred))
        }
        (Ty::InProjection(provider), Ty::InProjection(inferred)) => {
            Ty::in_projection(merge_specialized_return(*provider, *inferred))
        }
        (Ty::OutProjection(provider), Ty::OutProjection(inferred)) => {
            Ty::out_projection(merge_specialized_return(*provider, *inferred))
        }
        (Ty::StarProjection(provider), Ty::StarProjection(inferred)) => {
            Ty::star_projection(merge_specialized_return(*provider, *inferred))
        }
        (Ty::Obj(provider_name, provider_args), Ty::Obj(inferred_name, inferred_args))
            if provider_name == inferred_name =>
        {
            if provider_args.is_empty() {
                return Ty::obj_args_name(provider_name, inferred_args);
            }
            if inferred_args.is_empty() || provider_args.len() != inferred_args.len() {
                return Ty::obj_args_name(provider_name, provider_args);
            }
            let args = provider_args
                .iter()
                .zip(inferred_args)
                .map(|(&provider, &inferred)| merge_specialized_return(provider, inferred))
                .collect::<Vec<_>>();
            Ty::obj_args_name(provider_name, &args)
        }
        _ => provider,
    }
}
