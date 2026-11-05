//! Declaration-owned constructor argument shapes and contextual inference admission.

use super::*;

/// What `Name(args)` types its arguments against when the name denotes a constructible classifier.
///
/// Both parameter vectors are in SOURCE-ARGUMENT order — labels have already been mapped onto
/// declaration slots — so index `i` always describes the `i`th written argument.
pub(super) struct ConstructionExpectations {
    /// Per source argument: the parameter's semantic type and whether that type is a RECEIVER
    /// function type (`Scope.() -> Unit`, whose lambda binds `this`). `None` where same-arity
    /// constructors disagree, so the argument keeps its expectation-free shape.
    pub(super) params: Vec<Option<(Ty, bool)>>,
    /// The same parameters with the classifier's own type variables still symbolic, where the
    /// declaration provides them. This is the template call-site type arguments substitute into.
    pub(super) symbolic: Vec<Option<Ty>>,
    /// The constructed classifier's type parameters, in declaration order.
    pub(super) type_params: Vec<crate::types::TyParamName>,
    /// Declaration bounds parallel to `type_params`, used when an expected applied supertype
    /// supplies the classifier arguments before a contextual constructor argument is checked.
    pub(super) type_param_bounds: Vec<Vec<Ty>>,
    /// Declaration signatures of every constructor contributing to the mapped consensus.
    signatures: Vec<Option<crate::libraries::GenericSig>>,
}

impl Checker<'_> {
    /// The constructor shapes each argument of `Name(args)` is typed against, in SOURCE-ARGUMENT
    /// order. Providers normalize every declaration origin into the classifier's constructor list;
    /// ordinary callable mapping handles labels, defaults, varargs, and trailing lambdas once.
    ///
    /// The symbolic vector is the same mapped parameter list before the classifier's type variables
    /// are substituted. A slot is exposed only when every applicable constructor agrees, so this
    /// pre-selection expectation cannot choose an overload.
    pub(super) fn construction_argument_expectations(
        &self,
        internal: TypeName,
        arity: usize,
        arg_names: Option<&[Option<String>]>,
        trailing_lambda: bool,
    ) -> Option<ConstructionExpectations> {
        let classifier = self.resolved_type_name(internal)?;
        let declarations = self.constructor_declarations(internal, &classifier);
        let mut signatures = Vec::new();
        let candidates = declarations
            .iter()
            .filter_map(|constructor| {
                let slots = call_argument_parameter_indices(
                    arity,
                    constructor.params.len(),
                    arg_names,
                    trailing_lambda,
                    &constructor.call_sig,
                )?;
                let signature = crate::libraries::constructor_generic_signature(
                    internal,
                    &classifier,
                    constructor,
                );
                let symbolic = signature
                    .as_ref()
                    .filter(|generic| generic.params.len() == constructor.params.len());
                let parameters = slots
                    .into_iter()
                    .map(|slot| {
                        let parameter = (
                            constructor.params[slot],
                            constructor
                                .call_sig
                                .lambda_receiver_params
                                .get(slot)
                                .copied()
                                .unwrap_or(false),
                        );
                        let symbolic = symbolic.map(|generic| generic.params[slot]);
                        (parameter, symbolic)
                    })
                    .collect::<Vec<_>>();
                signatures.push(signature);
                Some(parameters)
            })
            .collect::<Vec<_>>();
        // A functional interface exposes a language-level single-argument constructor even though
        // its classifier has no JVM `<init>` declaration. Publish that synthetic declaration through
        // the same expectation table as ordinary constructors so lambdas and callable references are
        // contextually checked before either can emit diagnostics from an expectation-free probe.
        if candidates.is_empty()
            && arity == 1
            && arg_names.is_none_or(|names| names.iter().all(Option::is_none))
        {
            let sam = self.semantic_sam_signature(Ty::obj_name(internal))?;
            let function = Ty::fun_with_shape(
                sam.params,
                sam.ret,
                sam.context_count,
                sam.has_receiver,
                sam.suspend,
            );
            return Some(ConstructionExpectations {
                params: vec![Some((function, false))],
                symbolic: vec![Some(function)],
                type_params: crate::types::TyParamName::all(&classifier.type_params),
                type_param_bounds: classifier.type_param_bounds().to_vec(),
                signatures: Vec::new(),
            });
        }
        let first = candidates.first()?;
        Some(ConstructionExpectations {
            params: first
                .iter()
                .enumerate()
                .map(|(source, &(expectation, _))| {
                    candidates
                        .iter()
                        .all(|candidate| candidate[source].0 == expectation)
                        .then_some(expectation)
                })
                .collect(),
            symbolic: first
                .iter()
                .enumerate()
                .map(|(source, &(_, symbolic))| {
                    candidates
                        .iter()
                        .all(|candidate| candidate[source].1 == symbolic)
                        .then_some(symbolic)
                        .flatten()
                })
                .collect(),
            type_params: crate::types::TyParamName::all(&classifier.type_params),
            type_param_bounds: classifier.type_param_bounds().to_vec(),
            signatures,
        })
    }

    /// Bind the constructed classifier's type parameters from everything known at the call BEFORE a
    /// contextual (lambda) argument is checked: explicit type arguments, the expected result, and
    /// the arguments already typed. `actuals` holds `Ty::Error` for a slot not yet typed.
    ///
    /// This is the constructor half of ordinary bidirectional inference. Lambda input shaping and
    /// admitted nested result constraints use it: `Store<ROOT, DOMAIN>(wrapper: Wrapper<ROOT>, toDomain: (ROOT) -> DOMAIN)`
    /// must hand `{ dto -> … }` a `dto: RootDto`, not the unbound `ROOT` (which erases to its bound
    /// and fails every member read in the body).
    pub(super) fn construction_type_param_binds(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        expected: Option<Ty>,
        plan: &ConstructionExpectations,
        actuals: &[Ty],
    ) -> crate::symbol_resolver::GSigBinds {
        let mut binds = crate::symbol_resolver::GSigBinds::default();
        if plan.type_params.is_empty() {
            return binds;
        }
        let fixed = self.constructor_fixed_bindings(scope, call, internal, expected, plan);
        for (slot, shape) in plan.symbolic.iter().enumerate() {
            let (Some(shape), Some(&actual)) = (*shape, actuals.get(slot)) else {
                continue;
            };
            if actual == Ty::Error || !shape.mentions_ty_param() {
                continue;
            }
            crate::symbol_resolver::unify_inferred_ty(shape, actual, &mut binds);
        }
        // Written and expected applications are fixed independently of argument inference.
        binds.extend(fixed);
        binds.retain(|name, ty| {
            plan.type_params.iter().any(|formal| formal == name) && *ty != Ty::Error
        });
        binds
    }

    fn constructor_fixed_bindings(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        expected: Option<Ty>,
        plan: &ConstructionExpectations,
    ) -> crate::symbol_resolver::GSigBinds {
        let mut expected_bindings = crate::symbol_resolver::GSigBinds::default();
        {
            let source = self.fed_source();
            crate::symbol_resolver::seed_unbound_constructor_result_from_symbols(
                &source,
                internal,
                &plan.type_params,
                &plan.type_param_bounds,
                expected,
                &mut expected_bindings,
            );
        }
        let mut explicit = crate::symbol_resolver::GSigBinds::default();
        if let Some(refs) = self
            .file
            .call_type_args
            .get(&call.0)
            .map(|syntax| syntax.arguments.clone())
        {
            for (name, type_ref) in plan.type_params.iter().zip(refs.iter()) {
                let ty = self.type_ref_ty_silent(scope, type_ref);
                if ty != Ty::Error {
                    explicit.insert(crate::types::TyParamName::new(name), ty);
                }
            }
        }
        expected_bindings.extend(explicit);
        expected_bindings
    }

    /// A rejected class application cannot supply result constraints to a nested producer.
    /// This does not retract declaration input types, including explicit lambda parameter context.
    pub(super) fn constructor_result_context_admitted(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        expected: Option<Ty>,
        plan: &ConstructionExpectations,
    ) -> bool {
        if plan.signatures.is_empty() || plan.type_params.is_empty() {
            return true;
        }
        let fixed = self.constructor_fixed_bindings(scope, call, internal, expected, plan);
        let Some(arguments) = plan
            .type_params
            .iter()
            .map(|formal| fixed.get(formal.as_str()).copied())
            .collect::<Option<Vec<_>>>()
        else {
            // An open application still needs ordinary argument inference and selection.
            return true;
        };
        !plan.signatures.iter().all(|signature| {
            signature.as_ref().is_some_and(|signature| {
                self.explicit_constructor_bound_violation(&plan.type_params, signature, &arguments)
                    .is_some()
            })
        })
    }

    /// Context shared by the qualified source and provider constructor selectors. Only a fixed
    /// declaration parameter agreed by the whole constructor family supplies literal context.
    pub(super) fn contextualize_constructor_arrays(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        arguments: &[ExprId],
        names: Option<&[Option<String>]>,
    ) -> Vec<(usize, Ty)> {
        let Some(expectations) = self.construction_argument_expectations(
            internal,
            arguments.len(),
            names,
            self.file.call_has_trailing_lambda.contains(&call.0),
        ) else {
            return Vec::new();
        };
        arguments
            .iter()
            .enumerate()
            .filter_map(|(index, &argument)| {
                let declared = expectations.symbolic[index]
                    .or_else(|| expectations.params[index].map(|(ty, _)| ty))?;
                let actual =
                    self.annotation_array_declaration_context(scope, argument, declared)?;
                Some((index, actual))
            })
            .collect()
    }
}
