//! Lambda call-shape derivation over a candidate family.
//!
//! Before a call's lambdas have types, each candidate that can take the already typed arguments
//! supplies the parameter, receiver and expected function types its lambdas would be checked
//! against. Selecting the callable is left to call resolution once the lambdas are checked.

use super::*;

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
                let mut binds = std::collections::HashMap::new();
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
        if generic.as_ref().is_some_and(|(_, bindings)| {
            // This is the pre-lambda applicability probe. A bound that still depends on an
            // unbound callable formal is completed by the postponed lambda result, so it cannot
            // eliminate this overload yet. Final selection validates the complete signature.
            let unresolved = semantic
                .formals
                .iter()
                .filter(|formal| !bindings.contains_key(*formal))
                .cloned()
                .collect::<Vec<_>>();
            let mut partial_signature = semantic.clone().into_owned();
            for bounds in &mut partial_signature.formal_bounds {
                bounds.retain(|bound| !ty_mentions_param(*bound, &unresolved));
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
                            );
                        };
                        let Some(expected) = expected else {
                            return false;
                        };
                        if let Ty::Fun(signature) = expected.non_null() {
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
                        let expected_arity = match expected.non_null() {
                            Ty::Fun(signature) => Some(
                                signature.params.len()
                                    - signature.context_count.min(signature.params.len())
                                    - usize::from(signature.has_receiver),
                            ),
                            shape => self.semantic_sam_signature(shape).map(|sam| {
                                sam.params.len()
                                    - sam.context_count.min(sam.params.len())
                                    - usize::from(sam.has_receiver)
                            }),
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

    /// A postponed receiver-qualified callable reference is candidate evidence once the candidate
    /// supplies a concrete function shape, exactly as in overload ranking: `replaceFirstChar` has
    /// `(Char) -> Char` and `(Char) -> CharSequence` overloads, and `Char::uppercase` adapts only
    /// to the second. Shaping the reference under an overload it cannot fit types it against the
    /// wrong expectation and reports a reference the selected overload resolves. Any other
    /// argument, or a shape that still mentions a type parameter, constrains nothing here.
    fn postponed_callable_reference_fits(
        &self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        expected: Option<Ty>,
    ) -> bool {
        let Expr::CallableRef {
            receiver: Some(receiver),
            name,
        } = self.file.expr(argument)
        else {
            return true;
        };
        let Some(expected) =
            expected.map(|expected| self.declared_function_semantic_type(expected))
        else {
            return true;
        };
        let Ty::Fun(expected_function) = expected.non_null() else {
            return true;
        };
        if expected.mentions_ty_param() {
            return true;
        }
        self.receiver_qualified_callable_reference_adapts_to(
            scope,
            *receiver,
            name,
            expected_function,
        )
        .unwrap_or(true)
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
            .collect::<std::collections::HashSet<_>>();
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
            let Some(Ty::Fun(function)) = argument_parameters.get(argument).map(|ty| ty.non_null())
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
                        let wrapper = match parameter {
                            Ty::Nullable(_) => Some(Ty::nullable as fn(Ty) -> Ty),
                            Ty::PlatformNullable(_) => Some(Ty::platform_nullable as fn(Ty) -> Ty),
                            _ => None,
                        };
                        let Ty::Fun(function) = parameter.non_null() else {
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
                    .filter(|parameter| matches!(parameter.non_null(), Ty::Fun(_)))
            })
            .collect::<Vec<_>>();
        let fixed_expected_types = expected_types
            .iter()
            .enumerate()
            .map(|(argument, expected)| {
                (*expected).filter(|parameter| {
                    let Ty::Fun(function) = parameter.non_null() else {
                        return false;
                    };
                    let declared_return = argument_parameters.get(argument).and_then(|parameter| {
                        match parameter.non_null() {
                            Ty::Fun(function) => Some(function.ret),
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
                        .is_some_and(|ty| matches!(ty.non_null(), Ty::Fun(_)))
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
                    .and_then(|parameter| match parameter.non_null() {
                        Ty::Fun(function) => Some(function),
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
            .map(|parameter| match parameter.non_null() {
                Ty::Fun(function) => Some((function.params.clone(), function.ret)),
                _ => self
                    .semantic_sam_signature(*parameter)
                    .map(|sam| (sam.params, sam.ret)),
            })
            .collect::<Vec<_>>();
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
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

    /// Lambda call-shape facts for a receiver-less top-level call from the first `overloads` member
    /// that takes it, aligned to the PARTIAL argument list (a generic HOF binds lambda parameter
    /// types from the already-typed non-lambda args).
    pub(super) fn top_level_lambda_shape(
        &self,
        lexical_scope: &CheckerScope<'_>,
        overloads: &[crate::libraries::FunctionInfo],
        call: UntypedLambdaCall<'_>,
    ) -> Option<crate::symbol_resolver::LambdaCallShape> {
        overloads
            .iter()
            .filter(|overload| overload.kind == crate::libraries::FnKind::TopLevel)
            .find_map(|overload| {
                self.top_level_overload_lambda_shape(lexical_scope, overload, call)
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

    pub(super) fn extension_lambda_shape(
        &self,
        scope: &CheckerScope<'_>,
        receiver: Ty,
        name: &str,
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
        let src = self.fed_source();
        let fs = crate::libraries::FunctionSet {
            overloads: self
                .resolver()
                .resolve_symbol(
                    crate::symbol_resolver::SymRecv::Value(receiver),
                    name,
                    &[],
                    &[],
                )
                .map(crate::symbol_resolver::Symbol::overloads)
                .unwrap_or_default()
                .into_iter()
                .filter(crate::libraries::FunctionInfo::is_extension)
                .collect(),
        };
        crate::trace_compiler!(
            "lambda_shape",
            "extension inventory={name} receiver={receiver:?} candidates={} ranked={}",
            fs.overloads.len(),
            crate::symbol_resolver::ranked_extension_overloads_by_recv(&src, receiver, &fs).len(),
        );
        for (_, binding_receiver, o) in
            crate::symbol_resolver::ranked_extension_overloads_by_recv(&src, receiver, &fs)
        {
            crate::trace_compiler!(
                "lambda_shape",
                "extension candidate={name} receiver={receiver:?} scope_rank={} receiver_rank={} binding_receiver={binding_receiver:?} params={:?} generic={:?} partial={arg_tys:?}",
                o.scope_rank,
                o.receiver_rank,
                o.semantic_params(),
                o.generic_sig,
            );
            // Use the same semantic slot mapper as module/provider member expectations.
            // This handles reordered names, omitted defaults, and syntactic trailing lambdas
            // together; an origin-specific label-only map used to drift from real call
            // recording whenever more than one of those features appeared in the same call.
            let shape = self.contextual_call_shape(
                scope,
                &o.semantic_params(),
                &o.call_sig,
                o.context_count,
                arg_names,
            )?;
            let Some(argument_map) = call_argument_parameter_indices(
                arg_tys.len(),
                shape.params.len(),
                arg_names,
                trailing_lambda,
                &shape.call_sig,
            ) else {
                continue;
            };
            let argument_map = argument_map
                .into_iter()
                .map(|parameter| shape.parameter_indices[parameter])
                .collect::<Vec<_>>();
            let partial = self.lambda_overload_partially_applicable(
                scope,
                o,
                Some(binding_receiver),
                (args, arg_tys),
                (
                    &argument_map,
                    &named_whole_array_varargs(&argument_map, arg_names, &o.call_sig),
                ),
                type_args,
            );
            if !partial {
                crate::trace_compiler!(
                    "lambda_shape",
                    "extension candidate={name} rejected before contextual typing"
                );
                continue;
            }
            if let Some(shape) = self.lambda_shape_for_overload(
                o,
                Some(binding_receiver),
                (args, arg_tys),
                (
                    &argument_map,
                    &named_whole_array_varargs(&argument_map, arg_names, &o.call_sig),
                ),
                type_args,
                CallResultConstraint::direct(expected_result),
            ) {
                crate::trace_compiler!(
                    "lambda_shape",
                    "extension candidate={name} selected shape={shape:?}"
                );
                return Some(shape);
            }
        }
        None
    }
}
