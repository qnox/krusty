//! Lambda call-shape derivation over a candidate family.
//!
//! Before a call's lambdas have types, each candidate that can take the already typed arguments
//! supplies the parameter, receiver and expected function types its lambdas would be checked
//! against. Selecting the callable is left to call resolution once the lambdas are checked.

use super::member_extension_selection::MemberExtensionLambdaShape;
use super::*;
use crate::types::TyKind;

/// A call whose lambda arguments are not typed yet, as a lambda's shape is derived from it.
#[derive(Clone, Copy)]
pub(super) struct UntypedLambdaCall<'a> {
    pub(super) args: &'a [ExprId],
    /// Argument types known before any lambda is checked; `None` for a lambda.
    pub(super) partial: &'a [Option<Ty>],
    pub(super) arg_names: Option<&'a [Option<String>]>,
    pub(super) trailing_lambda: bool,
    pub(super) type_args: &'a [Ty],
    /// The type the call's result is expected to have.
    pub(super) expected: Option<Ty>,
}

impl Checker<'_> {
    // Lambda call-SHAPE derivation over the resolver's candidate family. Candidate inventory is
    // intentionally separate from overload selection: the lambda slots do not have types yet, so
    // selecting a callable here would be circular. Final call resolution consumes the checked
    // arguments through the ordinary resolver seam.

    /// Parameter types for the lambda argument of a call selected by lambda return type
    /// (`Iterable<T>.sumOf { … }`), read from the selected overload family.
    pub(super) fn lambda_return_overload_param_types(
        &self,
        receiver: Ty,
        name: &str,
    ) -> Option<Vec<Ty>> {
        let src = self.fed_source();
        let mro = crate::symbol_resolver::ReceiverMro::new(&src, receiver);
        self.resolver()
            .top_level_candidates(name)
            .iter()
            .filter(|o| {
                o.is_extension()
                    && o.semantic_receiver()
                        .is_none_or(|dr| mro.rank(&src, dr).is_some())
            })
            .find_map(|o| {
                let semantic = o.semantic_signature();
                let mut binds = crate::fx_hash::HashMap::default();
                if let Some(recv_sig) = semantic.receiver {
                    crate::symbol_resolver::unify_ty(recv_sig, receiver, &mut binds);
                }
                semantic
                    .params
                    .first()
                    .map(|selector| {
                        crate::symbol_resolver::function_input_types(
                            &self.fed_source(),
                            *selector,
                            &binds,
                        )
                    })
                    .filter(|params| !params.is_empty())
            })
    }

    pub(super) fn lambda_overload_partially_applicable(
        &self,
        scope: &CheckerScope<'_>,
        overload: &crate::libraries::FunctionInfo,
        receiver: Option<Ty>,
        args_and_partial: (&[ExprId], &[Option<Ty>]),
        argument_slots: (&[usize], &[bool]),
        type_args: &[Ty],
    ) -> bool {
        let (args, arg_tys) = args_and_partial;
        let (argument_map, whole_array_varargs) = argument_slots;
        let semantic = overload.semantic_signature();
        if !type_args.is_empty() && semantic.formals.len() != type_args.len() {
            return false;
        }
        // A spread may already be typed as its element (`*xs: String`) or may still carry the
        // array (`*xs: Array<String>`), depending on when contextual typing ran. Whole-array
        // vararg syntax always contributes its element constraints to overload applicability.
        let normalized_arg_tys = argument_map
            .iter()
            .zip(arg_tys)
            .enumerate()
            .map(|(argument, (&parameter, actual))| {
                actual.map(|actual| {
                    if overload.call_sig.vararg_index == Some(parameter)
                        && whole_array_varargs.get(argument).copied().unwrap_or(false)
                    {
                        actual.non_null().array_read_elem().unwrap_or(actual)
                    } else {
                        actual
                    }
                })
            })
            .collect::<Vec<_>>();
        let argument_shapes = argument_map
            .iter()
            .zip(&normalized_arg_tys)
            .enumerate()
            .map(|(argument, (&parameter, actual))| {
                let declared = semantic.params.get(parameter).copied()?;
                if overload.call_sig.vararg_index == Some(parameter) {
                    let whole_array = whole_array_varargs.get(argument).copied().unwrap_or(false);
                    if let Some(element) = declared.array_read_elem() {
                        if whole_array {
                            return Some(element);
                        }
                        if actual.is_some_and(|actual| {
                            !crate::assignable::is_assignable(
                                &crate::assignable::TyCtx::new(),
                                self,
                                actual,
                                declared,
                            )
                        }) {
                            return Some(element);
                        }
                        // An untyped lambda packed into the vararg has no `actual` yet. Its
                        // declaration element may still be a bare formal (`vararg elements: T`),
                        // which becomes a function type only after explicit/earlier bindings are
                        // applied. Preserve the element shape here and let the binding step below
                        // decide callable-ness; using the array shape makes every multi-element
                        // `listOf<Receiver.() -> Unit> { ... }` lambda type receiver-less.
                        if actual.is_none()
                            && args.get(argument).is_some_and(|argument| {
                                matches!(self.file.expr(*argument), Expr::Lambda { .. })
                            })
                        {
                            return Some(element);
                        }
                    }
                }
                Some(declared)
            })
            .collect::<Option<Vec<_>>>();
        let generic = argument_shapes.as_ref().map(|argument_shapes| {
            let bindings = self.mapped_generic_call_bindings(
                overload,
                receiver,
                (args, arg_tys),
                argument_map,
                whole_array_varargs,
                type_args,
            );
            let params = argument_shapes
                .iter()
                .map(|&parameter_shape| {
                    crate::symbol_resolver::ty_subst(parameter_shape, &bindings)
                })
                .collect::<Vec<_>>();
            (params, bindings)
        });
        let unbound_formals = generic
            .as_ref()
            .map(|(_, bindings)| {
                semantic
                    .formals
                    .iter()
                    .filter(|formal| !bindings.contains_key(*formal))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if generic.as_ref().is_some_and(|(_, bindings)| {
            // This is the pre-lambda applicability probe. A bound that still depends on an
            // unbound callable formal is completed by the postponed lambda result, so it cannot
            // eliminate this overload yet. Final selection validates the complete signature.
            let mut partial_signature = semantic.clone().into_owned();
            for bounds in &mut partial_signature.formal_bounds {
                bounds.retain(|bound| !ty_mentions_param(*bound, &unbound_formals));
            }
            !crate::symbol_resolver::generic_bindings_satisfy_bounds(
                &partial_signature,
                bindings,
                |actual, bound| self.generic_bound_admits(actual, bound),
            )
        }) {
            return false;
        }
        let generic_params = generic.as_ref().map(|(params, _)| params);
        let callable_params = overload.semantic_params();
        argument_map
            .iter()
            .enumerate()
            .zip(&normalized_arg_tys)
            .all(|((argument, &parameter), actual)| {
                let declared = argument_shapes
                    .as_ref()
                    .and_then(|params| params.get(argument))
                    .copied()
                    .or_else(|| callable_params.get(parameter).copied());
                let expected = generic_params
                    .and_then(|params| params.get(argument))
                    .copied()
                    .or(declared);
                match *actual {
                    None => {
                        let Some(&argument_expr) = args.get(argument) else {
                            return true;
                        };
                        let Expr::Lambda { params, .. } = self.file.expr(argument_expr) else {
                            return self.postponed_callable_reference_fits(
                                scope,
                                argument_expr,
                                expected,
                                declared.is_some_and(|declared| {
                                    ty_mentions_param(declared, &unbound_formals)
                                }),
                            );
                        };
                        let Some(expected) = expected else {
                            return false;
                        };
                        if let TyKind::Fun(signature) = expected.non_null().kind() {
                            // A receiver-less anonymous function may spell an extension
                            // function's receiver as its first ordinary parameter:
                            // `fun(it) { it.member() }` adapts to `R.() -> Unit`. A lambda arrow
                            // under that expectation still has zero value parameters. Use the
                            // syntax-aware shared rule here so shaping and selected commitment
                            // agree on the same callable arity.
                            if self.file.anon_fun_lambdas.contains(&argument_expr.0) {
                                return self.contextual_lambda_accepts_function_shape(
                                    argument_expr,
                                    signature,
                                );
                            }
                        }
                        // kotlinc's arity rule for an unchecked lambda: the DECLARED (post-desugar)
                        // parameter count must equal the expected shape's value-parameter count —
                        // `{ (k, v) -> }` is ONE destructured parameter, so it is not pertinent to a
                        // two-parameter function/SAM shape, while a lambda with no declared
                        // parameters still fits arity 0/1 (implicit `it`). Context and receiver
                        // slots are not value parameters a lambda arrow declares.
                        let expected_arity = {
                            let shape = expected.non_null();
                            match shape.kind() {
                                TyKind::Fun(signature) => Some(
                                    signature.params.len()
                                        - signature.context_count.min(signature.params.len())
                                        - usize::from(signature.has_receiver),
                                ),
                                _ => self.semantic_sam_signature(shape).map(|sam| {
                                    sam.params.len()
                                        - sam.context_count.min(sam.params.len())
                                        - usize::from(sam.has_receiver)
                                }),
                            }
                        };
                        match expected_arity {
                            Some(arity) => {
                                if params.is_empty() {
                                    arity <= 1
                                } else {
                                    let named_contexts = self
                                        .file
                                        .anon_fun_context_count
                                        .get(&argument_expr.0)
                                        .copied()
                                        .unwrap_or(0)
                                        as usize;
                                    params.len() == arity + named_contexts
                                }
                            }
                            None => expected.is_erased_top(),
                        }
                    }
                    Some(actual) => {
                        // This predicate only keeps candidates alive while postponed lambdas are
                        // shaped. Real overload inference owns type-variable constraint merging;
                        // rejecting a generic shape here would duplicate that selector and cannot
                        // model variance/LUB constraints (`Sink<Int>`, `Sink<String>`, `Sink<Long>`).
                        if let Some(declared) = declared
                            .filter(|declared| ty_mentions_param(*declared, &semantic.formals))
                        {
                            return self.postponed_argument_fits(
                                declared,
                                actual,
                                &semantic.formals,
                            );
                        }
                        expected.is_some_and(|expected| {
                            crate::assignable::is_assignable(
                                &crate::assignable::TyCtx::new(),
                                self,
                                actual,
                                expected,
                            ) || (!expected.is_reference()
                                && arg_assignable_simple(expected, actual))
                        })
                    }
                }
            })
    }

    /// A postponed callable reference is candidate evidence once the candidate supplies a concrete
    /// function shape, exactly as in overload ranking: `replaceFirstChar` has `(Char) -> Char` and
    /// `(Char) -> CharSequence` overloads, and `Char::uppercase` adapts only to the second. The same
    /// applies to a receiver-less overloaded reference such as `::shout`. Shaping the reference
    /// under an overload it cannot fit types it against the wrong expectation and reports a
    /// reference the selected overload resolves. Any other argument, or a shape that still mentions
    /// a currently unbound callee formal, constrains nothing here. That fact is read from the
    /// declared shape before substitution can erase the formal to its upper bound.
    fn postponed_callable_reference_fits(
        &self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        expected: Option<Ty>,
        declared_has_unbound_formal: bool,
    ) -> bool {
        let Expr::CallableRef { receiver, name } = self.file.expr(argument) else {
            return true;
        };
        if declared_has_unbound_formal {
            return true;
        }
        let Some(expected) =
            expected.map(|expected| self.declared_function_semantic_type(expected))
        else {
            return true;
        };
        let TyKind::Fun(expected_function) = expected.non_null().kind() else {
            return true;
        };
        if expected.mentions_ty_param() {
            return true;
        }
        receiver.map_or_else(
            || self.callable_reference_adapts_to(scope, argument, expected),
            |receiver| {
                self.receiver_qualified_callable_reference_adapts_to(
                    scope,
                    receiver,
                    name,
                    expected_function,
                )
                .unwrap_or(true)
            },
        )
    }

    pub(super) fn lambda_shape_for_overload(
        &self,
        overload: &crate::libraries::FunctionInfo,
        receiver: Option<Ty>,
        args_and_partial: (&[ExprId], &[Option<Ty>]),
        mapped_arguments: (&[usize], &[bool]),
        type_args: &[Ty],
        result_constraint: CallResultConstraint,
    ) -> Option<crate::symbol_resolver::LambdaCallShape> {
        let (args, arg_tys) = args_and_partial;
        let (argument_map, whole_array_varargs) = mapped_arguments;
        let mut shape = crate::symbol_resolver::LambdaCallShape::default();
        let semantic = overload.semantic_signature();
        let result_semantic = result_constraint.signature(&semantic);
        let argument_parameters = argument_map
            .iter()
            .enumerate()
            .map(|(argument, &parameter)| {
                let declared = semantic.params.get(parameter).copied()?;
                let packed_element = overload.call_sig.vararg_index == Some(parameter)
                    && !whole_array_varargs.get(argument).copied().unwrap_or(false)
                    && !args
                        .get(argument)
                        .is_some_and(|argument| self.file.is_spread_arg(*argument));
                Some(if packed_element {
                    declared.array_read_elem().unwrap_or(declared)
                } else {
                    declared
                })
            })
            .collect::<Option<Vec<_>>>()?;
        shape.generic_formals = semantic.formals.clone();
        shape.argument_parameters = argument_parameters.clone();
        let mut binds = crate::symbol_resolver::seeded_gsig_binds(&semantic, type_args);
        let mut fixed_expectation_formals = semantic
            .formals
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                type_args
                    .get(*index)
                    .is_some_and(|argument| *argument != Ty::Error)
            })
            .map(|(_, formal)| formal)
            .cloned()
            .collect::<crate::fx_hash::HashSet<_>>();
        if let (Some(receiver), Some(receiver_sig)) = (receiver, semantic.receiver) {
            crate::symbol_resolver::unify_ty_from_symbols(
                &self.fed_source(),
                receiver_sig,
                receiver,
                &mut binds,
            );
            // A receiver contributes constraints, not necessarily equalities. For
            // `(() -> Nothing).recover { "OK" }`, the receiver only establishes the lower bound
            // `R :> Nothing`; pre-typing the second lambda as `() -> Nothing` would make overload
            // shaping perform final inference. Conversely, `MutableMap<K, V>` is invariant, so its
            // applied `V` is fixed and may contextually type `getOrPut { ArrayList() }`.
            //
            // Ask assignability instead of duplicating variance traversal: if replacing one inferred
            // formal by its broad declared bound still admits the actual receiver, that binding can
            // widen and is not an expected lambda-return type yet.
            for (index, formal) in semantic.formals.iter().enumerate() {
                if fixed_expectation_formals.contains(formal) || !binds.contains_key(formal) {
                    continue;
                }
                let broad = semantic
                    .formal_bounds
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                let mut trial = binds.clone();
                trial.insert(formal.clone(), broad);
                let widened_receiver =
                    crate::symbol_resolver::ty_subst_keep_unbound(receiver_sig, &trial);
                let widened_receiver = self.declared_function_semantic_type(widened_receiver);
                let widens = self.receiver_is_assignable(receiver, widened_receiver);
                crate::trace_compiler!(
                    "expected_call",
                    "receiver lambda constraint formal={formal:?} binding={:?} broad={broad:?} receiver={receiver:?} declared={receiver_sig:?} widened={widened_receiver:?} widens={widens}",
                    binds.get(formal),
                );
                if !widens {
                    fixed_expectation_formals.insert(formal.clone());
                }
            }
        }
        // A receiver binding that remains widenable in a covariant shell can still become fixed
        // contextual input for the lambda. `Flow<out Int>.flatMap((Int) -> Flow<Int>)` supplies
        // `Int` as the literal's parameter type; that input is not inferred from the lambda body,
        // and therefore also fixes occurrences of the same formal in its expected result. A
        // result-only formal (`recover { "OK" }` with `() -> R`) remains deliberately unfixed.
        for (argument, actual) in arg_tys.iter().enumerate() {
            if actual.is_some() {
                continue;
            }
            let Some(TyKind::Fun(function)) = argument_parameters
                .get(argument)
                .map(|ty| ty.non_null())
                .map(Ty::kind)
            else {
                continue;
            };
            for formal in &semantic.formals {
                if binds.contains_key(formal)
                    && function
                        .params
                        .iter()
                        .copied()
                        .any(|input| ty_mentions_param(input, std::slice::from_ref(formal)))
                {
                    fixed_expectation_formals.insert(formal.clone());
                }
            }
        }
        let mut expectation_binds = binds.clone();
        expectation_binds.retain(|formal, _| {
            !semantic.formals.iter().any(|declared| declared == formal)
                || fixed_expectation_formals.contains(formal)
        });
        if let Some(expected) = result_constraint.expected() {
            for (formal, actual) in self
                .contextual_lambda_result_bindings(&result_semantic, expected)
                .ok()?
            {
                if !fixed_expectation_formals.contains(&formal) {
                    binds.insert(formal.clone(), actual);
                    expectation_binds.insert(formal.clone(), actual);
                    fixed_expectation_formals.insert(formal);
                }
            }
        }
        let fixed_before_argument_merge = expectation_binds.clone();
        self.merge_mapped_generic_argument_bindings(
            overload,
            receiver,
            (args, arg_tys),
            argument_map,
            whole_array_varargs,
            type_args,
            &mut binds,
        );
        // Written arguments contribute lower constraints, but an explicit type argument,
        // invariant receiver equality, or contextual result equality is already fixed. In
        // particular, `foldl(0) { e: Int, acc: Long -> ... }` under an expected `Long` must adapt
        // the integer literal to `Long`; joining the provisional `Int` with fixed `A = Long` into
        // `Any` would type the lambda's `acc` as `Any` before final overload selection.
        for formal in &fixed_expectation_formals {
            if let Some(&fixed) = fixed_before_argument_merge.get(formal) {
                binds.insert(formal.clone(), fixed);
            }
        }
        for (&parameter, actual) in argument_map.iter().zip(arg_tys) {
            if actual.is_some() {
                let Some(parameter) = semantic.params.get(parameter) else {
                    continue;
                };
                for formal in &semantic.formals {
                    if binds.contains_key(formal)
                        && crate::symbol_resolver::formal_variance_in_type(
                            &self.fed_source(),
                            *parameter,
                            formal,
                        ) == Some(crate::types::TypeVariance::Invariant)
                    {
                        fixed_expectation_formals.insert(formal.clone());
                    }
                }
            }
        }
        expectation_binds.extend(
            fixed_expectation_formals
                .iter()
                .filter_map(|formal| binds.get(formal).map(|binding| (formal.clone(), *binding))),
        );
        let param_types = argument_parameters
            .iter()
            .zip(argument_map)
            .map(|(&argument_parameter, &parameter)| {
                Some(argument_parameter)
                    .map(|parameter| {
                        crate::symbol_resolver::function_input_types(
                            &self.fed_source(),
                            parameter,
                            &binds,
                        )
                    })
                    .filter(|types| !types.is_empty())
                    .unwrap_or_else(|| {
                        overload
                            .call_sig
                            .lambda_param_types
                            .get(parameter)
                            .into_iter()
                            .flatten()
                            .map(|ty| {
                                crate::symbol_resolver::instantiate_slot(
                                    &self.fed_source(),
                                    Some(&semantic),
                                    *ty,
                                    &binds,
                                    crate::symbol_resolver::TypePosition::Out,
                                    crate::symbol_resolver::UnboundSpecialization::UseUpperBound,
                                )
                            })
                            .collect()
                    })
            })
            .collect::<Vec<_>>();
        let expected_types = argument_parameters
            .iter()
            .map(|&argument_parameter| {
                Some(argument_parameter)
                    .map(|parameter| {
                        let wrapper = match parameter.kind() {
                            TyKind::Nullable(_) => Some(Ty::nullable as fn(Ty) -> Ty),
                            TyKind::PlatformNullable(_) => {
                                Some(Ty::platform_nullable as fn(Ty) -> Ty)
                            }
                            TyKind::MutabilityFlexible(_) => {
                                Some(Ty::mutability_flexible as fn(Ty) -> Ty)
                            }
                            _ => None,
                        };
                        let TyKind::Fun(function) = parameter.non_null().kind() else {
                            return crate::symbol_resolver::ty_subst_keep_unbound(
                                parameter,
                                &expectation_binds,
                            );
                        };
                        // Receiver and ordinary-argument constraints already determine the callable
                        // INPUT shape used to select a reference (`List<Int>.map(::Boxed)` expects an
                        // `Int`, not the declaration's still-symbolic `T`). A covariant receiver may
                        // leave the callable RESULT widenable, so substitute that position only from
                        // the bindings proven fixed above (`(() -> Nothing).recover { "OK" }`).
                        let callable = Ty::fun_with_shape(
                            function
                                .params
                                .iter()
                                .map(|parameter| {
                                    crate::symbol_resolver::ty_subst_keep_unbound(
                                        *parameter, &binds,
                                    )
                                })
                                .collect(),
                            crate::symbol_resolver::ty_subst_keep_unbound(
                                function.ret,
                                &expectation_binds,
                            ),
                            function.context_count,
                            function.has_receiver,
                            function.suspend,
                        );
                        wrapper.map_or(callable, |wrap| wrap(callable))
                    })
                    // A still-symbolic result (`(String) -> R`) is already an authoritative
                    // callable shape: its input selects the reference/lambda overload, while the
                    // checked expression supplies the remaining `R` constraint. Dropping the whole
                    // expectation until every formal was bound made receiver calls type callable
                    // references without context and then fail before ordinary overload inference.
                    .filter(|parameter| parameter.non_null().is_fun())
            })
            .collect::<Vec<_>>();
        let fixed_expected_types = expected_types
            .iter()
            .enumerate()
            .map(|(argument, expected)| {
                (*expected).filter(|parameter| {
                    let TyKind::Fun(function) = parameter.non_null().kind() else {
                        return false;
                    };
                    let declared_return = argument_parameters.get(argument).and_then(|parameter| {
                        match parameter.non_null().kind() {
                            TyKind::Fun(function) => Some(function.ret),
                            _ => None,
                        }
                    });
                    let result_formals = semantic
                        .formals
                        .iter()
                        .filter(|formal| {
                            declared_return.is_some_and(|declared_return| {
                                ty_mentions_param(declared_return, std::slice::from_ref(formal))
                            })
                        })
                        .collect::<Vec<_>>();
                    // A fixed *callee formal* is authoritative contextual information (explicit
                    // type arguments, invariant receiver evidence, or the call's expected result).
                    // A concrete return baked into one overload is not: the lambda body must still
                    // participate in choosing among `sumOf`-style overloads with identical inputs.
                    !result_formals.is_empty()
                        && result_formals
                            .into_iter()
                            .all(|formal| fixed_expectation_formals.contains(formal))
                        && !ty_mentions_param(function.ret, &semantic.formals)
                })
            })
            .collect::<Vec<_>>();
        // Presence of a function-typed parameter is the expectation. Its input list may legitimately
        // be empty (`() -> T`); using `!parameters.is_empty()` erased that distinction and caused a
        // selected zero-argument inline lambda to be checked as an unlabelled standalone closure.
        if argument_map
            .iter()
            .zip(arg_tys)
            .enumerate()
            .any(|(argument, (_, actual))| {
                actual.is_none()
                    && argument_parameters
                        .get(argument)
                        .copied()
                        .map(|ty| crate::symbol_resolver::ty_subst_keep_unbound(ty, &binds))
                        // A generic parameter can become a function type only after receiver or
                        // earlier-argument constraints are applied (`MyList<T>.add(T)` on a
                        // `MyList<(Int) -> Int>`). Callable-ness belongs to that instantiated
                        // semantic parameter, not to the declaration's bare `T` node.
                        .is_some_and(|ty| ty.non_null().is_fun())
            })
        {
            shape.param_types = Some(param_types);
            shape.expected_types = Some(expected_types);
            shape.fixed_expected_types = Some(fixed_expected_types);
        }
        let receivers = argument_map
            .iter()
            .enumerate()
            .map(|(argument, &parameter)| {
                // The receiver comes from the parameter's own function TYPE (already substituted into
                // `param_types`), which carries the receiver's type ARGUMENTS (`Config<T>.() -> Unit` with
                // `T` bound by this call). `call_sig.lambda_receivers` is the weaker fallback: a callable
                // with no generic signature records only the receiver's CLASS, so preferring it would
                // shape the lambda against a raw `Config` and lose the binding.
                //
                // A `Ty::Fun` whose `has_receiver` is set is the structured form of the same metadata
                // fact carried by `lambda_receiver_params`.
                let instantiated_function = shape
                    .expected_types
                    .as_ref()
                    .and_then(|types| types.get(argument))
                    .copied()
                    .flatten()
                    .or_else(|| {
                        argument_parameters.get(argument).copied().map(|parameter| {
                            crate::symbol_resolver::ty_subst_keep_unbound(parameter, &binds)
                        })
                    })
                    .and_then(|parameter| match parameter.non_null().kind() {
                        TyKind::Fun(function) => Some(function),
                        _ => None,
                    });
                let receiver_fun_param = overload
                    .call_sig
                    .lambda_receiver_params
                    .get(parameter)
                    .copied()
                    .unwrap_or(false)
                    || instantiated_function.is_some_and(|function| function.has_receiver);
                receiver_fun_param
                    .then(|| {
                        let context_count = instantiated_function.map_or_else(
                            || {
                                overload
                                    .call_sig
                                    .lambda_context_counts
                                    .get(parameter)
                                    .copied()
                                    .unwrap_or_default()
                            },
                            |function| function.context_count,
                        );
                        instantiated_function
                            .and_then(|function| {
                                function.params.get(function.context_count).copied()
                            })
                            .or_else(|| {
                                shape
                                    .param_types
                                    .as_ref()
                                    .and_then(|types| types.get(argument))
                                    .and_then(|types| types.get(context_count))
                                    .copied()
                            })
                    })
                    .flatten()
                    .or_else(|| {
                        overload
                            .call_sig
                            .lambda_receivers
                            .get(parameter)
                            .copied()
                            .flatten()
                            .map(|receiver| crate::symbol_resolver::ty_subst(receiver, &binds))
                    })
            })
            .collect::<Vec<_>>();
        shape.receivers = Some(receivers);
        shape.context_counts = Some(
            argument_map
                .iter()
                .map(|&parameter| {
                    overload
                        .call_sig
                        .lambda_context_counts
                        .get(parameter)
                        .copied()
                        .unwrap_or_default()
                })
                .collect(),
        );
        shape.boxes_captures = Some(
            argument_map
                .iter()
                .map(|&parameter| {
                    overload
                        .call_sig
                        .inline_modifiers
                        .get(parameter)
                        .map(|inlining| inlining.boxes_captures())
                })
                .collect(),
        );
        shape.inline = overload.flags.inline.can_inline();
        crate::trace_compiler!(
            "lambda_shape",
            "callable={} type_args={type_args:?} params={:?} argument_map={argument_map:?} receiver_marks={:?} receiver_types={:?} shape={shape:?}",
            overload.callable.name,
            semantic.params,
            overload.call_sig.lambda_receiver_params,
            overload.call_sig.lambda_receivers,
        );
        (shape.param_types.is_some()
            || shape
                .expected_types
                .as_ref()
                .is_some_and(|items| items.iter().any(Option::is_some))
            || shape
                .receivers
                .as_ref()
                .is_some_and(|items| items.iter().any(Option::is_some))
            || shape
                .context_counts
                .as_ref()
                .is_some_and(|items| items.iter().any(|count| *count > 0))
            || shape
                .boxes_captures
                .as_ref()
                .is_some_and(|items| items.iter().any(Option::is_some)))
        .then_some(shape)
    }

    pub(super) fn contextual_lambda_result_bindings(
        &self,
        signature: &crate::libraries::GenericSig,
        expected: Ty,
    ) -> Result<crate::symbol_resolver::GSigBinds, String> {
        crate::trace_compiler!(
            "expected_call",
            "contextual lambda result declared={:?} expected={expected:?}",
            signature.ret,
        );
        let symbolic = crate::symbol_resolver::infer_generic_symbolic_return_constraints(
            signature.ret,
            expected,
            &signature.formals,
        );
        if !symbolic.conflicting_formals.is_empty() {
            crate::trace_compiler!(
                "expected_call",
                "contextual lambda result conflicts={:?}",
                symbolic.conflicting_formals,
            );
            return Err(symbolic
                .conflicting_formals
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "?".to_string()));
        }
        let lambda_expectations = signature
            .params
            .iter()
            .map(|parameter| match parameter.non_null().kind() {
                TyKind::Fun(function) => Some((function.params.clone(), function.ret)),
                _ => self
                    .semantic_sam_signature(*parameter)
                    .map(|sam| (sam.params, sam.ret)),
            })
            .collect::<Vec<_>>();
        let mut bindings = crate::symbol_resolver::GSigBinds::default();
        for (formal, actual) in symbolic.bindings {
            let shapes_lambda_expectation =
                lambda_expectations
                    .iter()
                    .flatten()
                    .any(|(inputs, result)| {
                        ty_mentions_param(*result, std::slice::from_ref(&formal))
                            || inputs.iter().any(|input| {
                                ty_mentions_param(*input, std::slice::from_ref(&formal))
                            })
                    });
            if shapes_lambda_expectation {
                bindings.entry(formal).or_insert(actual);
            }
        }
        if let Some(inferred) = crate::symbol_resolver::infer_generic_return_bindings(
            signature,
            expected,
            |actual, bound| self.generic_bound_admits(actual, bound),
        ) {
            for (formal, actual) in inferred {
                if !symbolic.constrained_formals.contains(&formal) {
                    bindings.entry(formal).or_insert(actual);
                }
            }
        }
        crate::trace_compiler!(
            "expected_call",
            "contextual lambda result bindings={bindings:?}",
        );
        Ok(bindings)
    }

    /// The lambda call shape of a receiver-less call: a top-level overload's, else, for a semantic
    /// `Type(args) { … }` factory, the companion's `operator fun invoke`, whose (suspend) receiver
    /// function type shapes the lambda the same way; without it the block types receiver-less and
    /// every member inside is unresolved. A lambda that declares no parameter types takes its
    /// inputs from the call's candidate; when applicable top-level candidates expect different
    /// inputs for it, kotlinc cannot analyze the lambda before choosing, and the call is ambiguous
    /// between those candidates (`Err`).
    pub(super) fn receiverless_lambda_shape(
        &self,
        lexical_scope: &CheckerScope<'_>,
        overloads: &[crate::libraries::FunctionInfo],
        name: &str,
        call: UntypedLambdaCall<'_>,
    ) -> Result<Option<crate::symbol_resolver::LambdaCallShape>, Vec<crate::libraries::FunctionInfo>>
    {
        let shaped = overloads
            .iter()
            .filter(|overload| overload.kind == crate::libraries::FnKind::TopLevel)
            .filter_map(|overload| {
                self.top_level_overload_lambda_shape(lexical_scope, overload, call)
                    .map(|shape| (overload, shape))
            })
            .collect::<Vec<_>>();
        let Some((_, first)) = shaped.first() else {
            return Ok(self.companion_invoke_lambda_shape(
                lexical_scope,
                name,
                (call.args, call.partial),
                (call.arg_names, call.trailing_lambda),
                call.type_args,
                CallResultConstraint::direct(call.expected),
            ));
        };
        if shaped
            .iter()
            .any(|(_, other)| self.untyped_lambda_inputs_differ(call.args, first, other))
        {
            return Err(shaped
                .into_iter()
                .map(|(overload, _)| overload.clone())
                .collect());
        }
        Ok(shaped.into_iter().next().map(|(_, shape)| shape))
    }

    /// Report a call whose untyped lambda the applicable `candidates` would shape differently:
    /// kotlinc's overload ambiguity, and the lambda's own diagnostics as no candidate shapes it.
    pub(super) fn reject_ambiguous_lambda_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        name: &str,
        candidates: &[crate::libraries::FunctionInfo],
    ) -> Ty {
        self.report_callable_ambiguity(call, name, candidates);
        if let Expr::Call { args, .. } = self.file.expr(call) {
            let args = args.clone();
            // A lambda with neither parameters nor an implicit `it` is still checked, without a
            // receiver: kotlinc reports `{ length }` as `unresolved reference 'length'`.
            for &argument in &args {
                let Expr::Lambda { params, body } = self.file.expr(argument).clone() else {
                    continue;
                };
                if params.is_empty() && !self.untyped_lambda_binds_implicit_it(scope, body) {
                    self.omit_unshaped_any_receiver_type += 1;
                    self.expr_inner_lambda(scope, argument, None, params, body, Some(name));
                    self.omit_unshaped_any_receiver_type -= 1;
                }
            }
            self.report_unshaped_lambda_diagnostics(scope, &args);
        }
        self.set(call, Ty::Error)
    }

    /// Whether `left` and `right` give a lambda argument of `args` that declares no parameter
    /// types incomparable inputs: two concrete receivers, or two parameters at the same position of
    /// equally long lists, neither of which is a subtype of the other (`(Int) -> Unit` against
    /// `(String) -> Unit`, `Int.() -> Unit` against `String.() -> Unit`). Neither candidate is then
    /// more specific for that lambda.
    fn untyped_lambda_inputs_differ(
        &self,
        args: &[ExprId],
        left: &crate::symbol_resolver::LambdaCallShape,
        right: &crate::symbol_resolver::LambdaCallShape,
    ) -> bool {
        let parameters = |shape: &crate::symbol_resolver::LambdaCallShape, index: usize| {
            shape
                .param_types
                .as_ref()
                .and_then(|types| types.get(index))
                .cloned()
        };
        let receiver = |shape: &crate::symbol_resolver::LambdaCallShape, index: usize| {
            shape
                .receivers
                .as_ref()
                .and_then(|receivers| receivers.get(index))
                .copied()
                .flatten()
        };
        let subtype = |sub: Ty, sup: Ty| {
            crate::assignable::is_subtype(&crate::assignable::TyCtx::new(), self, sub, sup)
        };
        let incomparable = |left: Ty, right: Ty| {
            !left.mentions_ty_param()
                && !right.mentions_ty_param()
                && left != Ty::Error
                && right != Ty::Error
                && !subtype(left, right)
                && !subtype(right, left)
        };
        args.iter().enumerate().any(|(index, &argument)| {
            let Expr::Lambda { .. } = self.file.expr(argument) else {
                return false;
            };
            let declares_types = self
                .file
                .lambda_param_types
                .get(&argument.0)
                .is_some_and(|types| types.iter().any(Option::is_some));
            let (Some(left_parameters), Some(right_parameters)) =
                (parameters(left, index), parameters(right, index))
            else {
                return false;
            };
            let receivers_differ = matches!(
                (receiver(left, index), receiver(right, index)),
                (Some(left), Some(right)) if incomparable(left, right)
            );
            let parameters_differ = left_parameters.len() == right_parameters.len()
                && left_parameters
                    .iter()
                    .zip(&right_parameters)
                    .any(|(&left, &right)| incomparable(left, right));
            !declares_types && (receivers_differ || parameters_differ)
        })
    }

    /// The lambda call shape one receiver-less overload gives a call whose lambdas are untyped, or
    /// `None` when the overload cannot take the call's partially typed arguments.
    pub(super) fn top_level_overload_lambda_shape(
        &self,
        lexical_scope: &CheckerScope<'_>,
        o: &crate::libraries::FunctionInfo,
        call: UntypedLambdaCall<'_>,
    ) -> Option<crate::symbol_resolver::LambdaCallShape> {
        let UntypedLambdaCall {
            args,
            partial: arg_tys,
            arg_names,
            trailing_lambda,
            type_args,
            expected: expected_result,
        } = call;
        let semantic = o.semantic_signature();
        crate::trace_compiler!(
            "lambda_shape",
            "top-level shape candidate={} semantic={:?} call_sig={:?} arguments={}",
            o.callable.name,
            semantic.params,
            o.call_sig,
            arg_tys.len(),
        );
        let call_shape = self.contextual_call_shape(
            lexical_scope,
            &semantic.params,
            &o.call_sig,
            o.context_count,
            arg_names,
        )?;
        let argument_map = call_argument_parameter_indices(
            arg_tys.len(),
            call_shape.params.len(),
            arg_names,
            trailing_lambda,
            &call_shape.call_sig,
        )?
        .into_iter()
        .map(|parameter| call_shape.parameter_indices[parameter])
        .collect::<Vec<_>>();
        // EXPLICIT type arguments must match the overload's formal count exactly (kotlinc
        // rejects a partial list), so a mismatching overload cannot be the called one.
        if !type_args.is_empty() && semantic.formals.len() != type_args.len() {
            return None;
        }
        let whole_arrays = named_whole_array_varargs(&argument_map, arg_names, &o.call_sig);
        let partially_applicable = self.lambda_overload_partially_applicable(
            lexical_scope,
            o,
            None,
            (args, arg_tys),
            (&argument_map, &whole_arrays),
            type_args,
        );
        crate::trace_compiler!(
            "lambda_shape",
            "top-level candidate={} params={:?} args={arg_tys:?} argument_map={argument_map:?} partially_applicable={partially_applicable}",
            o.callable.name,
            o.semantic_params(),
        );
        if !partially_applicable {
            return None;
        }
        self.lambda_shape_for_overload(
            o,
            None,
            (args, arg_tys),
            (&argument_map, &whole_arrays),
            type_args,
            CallResultConstraint::direct(expected_result),
        )
    }

    /// The lambda shape of an extension call, decided by the call's own pre-lambda candidate
    /// selection ([`Self::select_callable_candidate`] over the receiver's extension rung, every
    /// lambda argument still unanalyzed): its callable-tower rungs, declaration priorities,
    /// argument mapping and omitted defaults. The selected candidate shapes the lambdas. A tie is
    /// refined by the lambda's result where the specification allows it, and otherwise claims the
    /// call as ambiguous.
    pub(super) fn extension_lambda_shape(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        receiver: Ty,
        name: &str,
        untyped: UntypedLambdaCall<'_>,
    ) -> MemberExtensionLambdaShape {
        let family = crate::libraries::FunctionSet {
            overloads: self
                .stable_receiver_callables(receiver, name)
                .functions()
                .iter()
                .filter(|function| function.is_extension())
                .cloned()
                .collect(),
        };
        // Only the candidates that can take the already typed arguments, callable references
        // included, enter the selection, exactly as each would be checked before its lambdas.
        let src = self.fed_source();
        let shaped =
            crate::symbol_resolver::ranked_extension_overloads_by_recv(&src, receiver, &family)
                .into_iter()
                .filter_map(|(_, binding_receiver, overload)| {
                    self.extension_overload_shape(scope, name, overload, binding_receiver, untyped)
                        .map(|shape| (overload.clone(), shape))
                })
                .collect::<Vec<_>>();
        // A lambda is admitted by its shape; any other argument still untyped is postponed
        // until the selected candidate supplies its expectation (`mapTo(HashSet()) { .. }`).
        let arg_tys = untyped
            .partial
            .iter()
            .zip(untyped.args)
            .map(|(ty, &argument)| match (ty, self.file.expr(argument)) {
                (Some(ty), _) => *ty,
                (None, Expr::Lambda { .. }) => Ty::Error,
                (None, _) => Ty::Pending,
            })
            .collect::<Vec<_>>();
        let selection = self.select_callable_candidate(
            scope,
            CallArgs {
                call,
                args: untyped.args,
                arg_tys: &arg_tys,
            },
            untyped.type_args,
            Some(receiver),
            CallResultConstraint::direct(untyped.expected),
            shaped
                .iter()
                .map(|(overload, _)| overload.clone())
                .collect(),
        );
        let positions = match selection {
            // Kotlin resolves a call whose only shaped candidate is inapplicable to that
            // candidate, and still analyzes the lambda against it; its diagnostic is reported by
            // the call's own selection.
            None if shaped.len() == 1 => vec![0],
            None => return MemberExtensionLambdaShape::Absent,
            Some(
                CallableCandidateSelection::Selected(selected)
                | CallableCandidateSelection::MissingContext(selected),
            ) => selected.candidate_index.into_iter().collect::<Vec<_>>(),
            Some(CallableCandidateSelection::Ambiguous(_, positions)) => positions,
        };
        crate::trace_compiler!(
            "lambda_shape",
            "extension selection={name} receiver={receiver:?} candidates={} shaped={} chosen={positions:?}",
            family.overloads.len(),
            shaped.len(),
        );
        let tied = positions
            .iter()
            .map(|&position| (shaped[position].1.clone(), &shaped[position].0))
            .collect::<Vec<_>>();
        match self.extension_tie_choice(untyped.args, tied) {
            ExtensionShapeChoice::Decided(shape) => shape,
            ExtensionShapeChoice::Refine { lambda, tied } => self.refined_extension_lambda_shape(
                scope,
                name,
                (lambda, untyped.args[lambda]),
                tied,
            ),
        }
    }

    /// The lambda shape `overload` supplies when it is selected for `call` on `binding_receiver`.
    fn extension_overload_shape(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        overload: &crate::libraries::FunctionInfo,
        binding_receiver: Ty,
        call: UntypedLambdaCall<'_>,
    ) -> Option<crate::symbol_resolver::LambdaCallShape> {
        let UntypedLambdaCall {
            args,
            partial: arg_tys,
            arg_names,
            trailing_lambda,
            type_args,
            expected: expected_result,
        } = call;
        // Use the same semantic slot mapper as module/provider member expectations. This handles
        // reordered names, omitted defaults, and syntactic trailing lambdas together.
        let shape = self.contextual_call_shape(
            scope,
            &overload.semantic_params(),
            &overload.call_sig,
            overload.context_count,
            arg_names,
        )?;
        let argument_map = call_argument_parameter_indices(
            arg_tys.len(),
            shape.params.len(),
            arg_names,
            trailing_lambda,
            &shape.call_sig,
        )?
        .into_iter()
        .map(|parameter| shape.parameter_indices[parameter])
        .collect::<Vec<_>>();
        let whole_arrays = named_whole_array_varargs(&argument_map, arg_names, &overload.call_sig);
        if !self.lambda_overload_partially_applicable(
            scope,
            overload,
            Some(binding_receiver),
            (args, arg_tys),
            (&argument_map, &whole_arrays),
            type_args,
        ) {
            crate::trace_compiler!(
                "lambda_shape",
                "extension candidate={name} rejected before contextual typing"
            );
            return None;
        }
        let shape = self.lambda_shape_for_overload(
            overload,
            Some(binding_receiver),
            (args, arg_tys),
            (&argument_map, &whole_arrays),
            type_args,
            CallResultConstraint::direct(expected_result),
        )?;
        crate::trace_compiler!("lambda_shape", "extension candidate={name} shape={shape:?}");
        Some(shape)
    }

    /// The shape of a tie that the lambda's result refines. The lambda is probed against the first
    /// tied candidate's fixed expectation, and the first candidate whose result accepts the
    /// probed results shapes the lambda with its own fixed expectation. Without fixed
    /// expectations, or when none accepts the results, the lambda is checked with an open result
    /// and call resolution refines the tie.
    fn refined_extension_lambda_shape(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        (position, lambda): (usize, ExprId),
        mut tied: Vec<crate::symbol_resolver::LambdaCallShape>,
    ) -> MemberExtensionLambdaShape {
        let expectations = tied
            .iter()
            .map(|shape| {
                let fixed = shape
                    .fixed_expected_types
                    .as_ref()?
                    .get(position)
                    .copied()??;
                match fixed.non_null().kind() {
                    TyKind::Fun(signature) => Some(signature),
                    _ => None,
                }
            })
            .collect::<Option<Vec<_>>>();
        let inputs = tied[0]
            .param_types
            .as_ref()
            .and_then(|inputs| inputs.get(position))
            .cloned()
            .unwrap_or_default();
        let chosen = expectations.and_then(|expectations| {
            self.refine_tie_by_lambda_results(scope, (lambda, &inputs), &expectations, Some(name))
        });
        crate::trace_compiler!("lambda_shape", "extension refined tie chosen={chosen:?}");
        let Some(chosen) = chosen else {
            let mut shape = tied.swap_remove(0);
            if let Some(fixed) = shape
                .fixed_expected_types
                .as_mut()
                .and_then(|fixed| fixed.get_mut(position))
            {
                *fixed = None;
            }
            return MemberExtensionLambdaShape::Selected(shape);
        };
        MemberExtensionLambdaShape::Selected(tied.swap_remove(chosen))
    }

    /// What a tie of the extension call's ordinary selection decides for its lambdas. Tied
    /// candidates that shape the lambdas identically shape them together. The lambda's result
    /// refines the tie only where the specification allows it
    /// (`@OverloadResolutionByLambdaReturnType` on a tied candidate, exactly one lambda argument,
    /// and tied function types with the same inputs). Any other tie is an ambiguity that claims
    /// the call without shaping its lambdas.
    fn extension_tie_choice(
        &self,
        args: &[ExprId],
        tied: Vec<(
            crate::symbol_resolver::LambdaCallShape,
            &crate::libraries::FunctionInfo,
        )>,
    ) -> ExtensionShapeChoice {
        let Some(((first, _), others)) = tied.split_first() else {
            return ExtensionShapeChoice::Decided(MemberExtensionLambdaShape::Absent);
        };
        if others.iter().all(|(shape, _)| shape == first) {
            return ExtensionShapeChoice::Decided(MemberExtensionLambdaShape::Selected(
                first.clone(),
            ));
        }
        let refinable = tied.iter().any(|(_, overload)| {
            overload
                .callable
                .annotations
                .contains(&crate::types::type_name(
                    "kotlin/OverloadResolutionByLambdaReturnType",
                ))
        });
        let mut lambdas = args
            .iter()
            .enumerate()
            .filter(|(_, &argument)| matches!(self.file.expr(argument), Expr::Lambda { .. }))
            .map(|(position, _)| position);
        let (Some(lambda), None) = (lambdas.next(), lambdas.next()) else {
            return ExtensionShapeChoice::Decided(MemberExtensionLambdaShape::Ambiguous);
        };
        let inputs = |shape: &crate::symbol_resolver::LambdaCallShape| {
            let parameter = shape
                .expected_types
                .as_ref()?
                .get(lambda)
                .copied()
                .flatten()?;
            match parameter.non_null().kind() {
                TyKind::Fun(signature) => Some((
                    signature.params.clone(),
                    signature.context_count,
                    signature.has_receiver,
                    signature.suspend,
                )),
                _ => None,
            }
        };
        let same_inputs = inputs(first).is_some_and(|first_inputs| {
            others
                .iter()
                .all(|(shape, _)| inputs(shape).as_ref() == Some(&first_inputs))
        });
        crate::trace_compiler!(
            "lambda_shape",
            "extension tie refinable={refinable} same_inputs={same_inputs}"
        );
        if !refinable || !same_inputs {
            return ExtensionShapeChoice::Decided(MemberExtensionLambdaShape::Ambiguous);
        }
        ExtensionShapeChoice::Refine {
            lambda,
            tied: tied.into_iter().map(|(shape, _)| shape).collect(),
        }
    }
}

/// What an extension call's pre-lambda selection decides for its lambdas.
enum ExtensionShapeChoice {
    Decided(MemberExtensionLambdaShape),
    /// A tie that the result of the lambda argument at `lambda` refines.
    Refine {
        lambda: usize,
        tied: Vec<crate::symbol_resolver::LambdaCallShape>,
    },
}
