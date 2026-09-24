//! Generic result templates of checked calls.
//!
//! Expected-type inference asks two questions about an already checked call: which generic result
//! shape an enclosing constraint may still solve (its template), and whether the call's current
//! result is still open. Both are answered from the selection the checker retained, never from the
//! callee spelling.
//!
//! A call made inside its callee's own declaration (`Tree(label)` inside `class Tree<T>`, or a
//! recursive `f(x)` inside `fun <T> f`) sees the callee's formals as ordinary lexical types, while
//! the call itself infers fresh variables for them
//! ([`crate::symbol_resolver::CallSiteVariables`]). Selection solves with the scope in hand; these
//! later queries have only the expression, so the call's lexical formals are recorded when it is
//! checked, and a template renames them to the call's fresh variables exactly as solving did.

use super::*;

impl Checker<'_> {
    /// Record the type-parameter identities lexically in scope at `call`.
    pub(super) fn record_call_site_lexical_formals(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
    ) {
        let lexical = scope.lexical_tparam_identities();
        if lexical.is_empty() {
            self.call_site_lexical_formals.remove(&call);
        } else {
            self.call_site_lexical_formals.insert(call, lexical);
        }
    }

    /// Whether callee formal `formal` is lexically in scope at `call`, and therefore names the
    /// enclosing declaration's fixed type in that call's checked types.
    fn lexically_fixed_at_call(&self, call: ExprId, formal: &str) -> bool {
        self.call_site_lexical_formals
            .get(&call)
            .is_some_and(|lexical| lexical.iter().any(|identity| identity == formal))
    }

    /// `signature` as the template of the one call `call`: formals lexically in scope there are
    /// renamed to that call's fresh variables, exactly as its constraints were solved.
    fn call_site_signature(&self, call: ExprId, signature: GenericSig) -> GenericSig {
        crate::symbol_resolver::CallSiteVariables::instantiate(&signature, |formal| {
            self.lexically_fixed_at_call(call, formal)
        })
        .map_or(signature, |instance| instance.signature().clone())
    }

    /// The semantic generic signature retained by one checker-selected call. Declaration origin and
    /// call spelling do not affect expected-result inference.
    pub(super) fn selected_generic_call_signature(
        &self,
        expression: ExprId,
    ) -> Option<&GenericSig> {
        match self.resolved_calls.get(&expression)? {
            ResolvedCall::Member(member) => member.member.generic_sig.as_ref(),
            ResolvedCall::TopLevel(call) => call.callable.generic_sig.as_deref(),
            ResolvedCall::Companion(member) => member.generic_sig.as_ref(),
            ResolvedCall::Extension(call) => call.callable.generic_sig.as_deref(),
            ResolvedCall::LocalFunction(call) => call.sig.generic_sig.as_ref(),
            ResolvedCall::MemberExtension { .. } => None,
        }
    }

    /// Generic result shape that an enclosing selected parameter may constrain. A call whose own
    /// arguments produced a concrete probe is still contextual (`Item(0)` can become `Item<Any>`
    /// when consumed by an invariant outer result), so this is intentionally broader than
    /// [`Self::unbound_call_result_signature`]. Constructor targets carry their stable selected
    /// identity separately from ordinary calls; rebuild only the classifier-owned result template
    /// from the normalized class model, without looking at the callee spelling.
    pub(super) fn expected_type_callable_signature(
        &self,
        expression: ExprId,
    ) -> Option<GenericSig> {
        if self
            .file
            .call_type_args
            .get(&expression.0)
            .is_some_and(|arguments| !arguments.is_empty())
        {
            return None;
        }
        if let Some(signature) = self.contextual_constructor_signatures.get(&expression) {
            return Some(signature.clone());
        }
        if let Some(signature) = self.selected_generic_call_signature(expression) {
            return ty_mentions_param(signature.ret, &signature.formals)
                .then(|| self.call_site_signature(expression, signature.clone()));
        }
        let owner = self.resolved_constructors.get(&expression)?.owner();
        let classifier = self.resolved_type_name(owner)?;
        if classifier.type_params().is_empty() {
            return None;
        }
        let arguments = classifier
            .type_params()
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                let bound = classifier
                    .type_param_bounds()
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                Ty::ty_param(formal, bound)
            })
            .collect::<Vec<_>>();
        let template = GenericSig {
            formals: classifier.type_params().clone(),
            formal_bounds: classifier.type_param_bounds().clone(),
            receiver: None,
            params: Vec::new(),
            ret: Ty::obj_args_name(owner, &arguments),
            return_policy: GenericReturnPolicy::Exact,
        };
        // The template is this call's own result shape. A formal that is lexically in scope at
        // the call names the enclosing declaration's fixed type in the checked result, so the
        // template carries the call's fresh variable for it, as constraint solving did.
        Some(self.call_site_signature(expression, template))
    }

    /// A selected call whose generic result still contains declaration-owned variables. Expected-type
    /// inference consumes this retained declaration; it never repeats lookup from the callee syntax.
    pub(super) fn unbound_call_result_signature(&self, expression: ExprId) -> Option<&GenericSig> {
        if self
            .file
            .call_type_args
            .get(&expression.0)
            .is_some_and(|arguments| !arguments.is_empty())
        {
            return None;
        }
        let selected = self.selected_generic_call_signature(expression);
        crate::trace_compiler!(
            "expected_call",
            "unbound call expression={expression:?} actual={:?} signature={:?} bounds={:?} resolved={:?}",
            self.expr_types[expression.0 as usize],
            selected.map(|signature| (&signature.formals, signature.ret)),
            selected.map(|signature| &signature.formal_bounds),
            self.resolved_call_type_args.get(&expression),
        );
        let signature =
            selected.filter(|signature| ty_mentions_param(signature.ret, &signature.formals))?;
        let actual = self.expr_types[expression.0 as usize];
        // A formal lexically in scope at this call (a recursive `f(x)` inside `fun <T> f`) is the
        // enclosing declaration's fixed type wherever it occurs in the checked result.
        let open_formals = signature
            .formals
            .iter()
            .filter(|formal| !self.lexically_fixed_at_call(expression, formal))
            .cloned()
            .collect::<Vec<_>>();
        if self
            .resolved_call_type_args
            .get(&expression)
            .is_some_and(|resolved| {
                signature
                    .formals
                    .iter()
                    .enumerate()
                    .filter(|(_, formal)| {
                        ty_mentions_param(signature.ret, std::slice::from_ref(*formal))
                    })
                    .all(|(index, _)| resolved.get(index).is_some_and(Option::is_some))
            })
            // Rechecking may have replaced a formerly contextual call with an expectation-free
            // probe. A retained type-argument record is not proof of completion when the current
            // semantic result again contains this callee's declaration variables; the enclosing
            // constraint round must be allowed to solve that live result.
            && !ty_mentions_param(actual, &open_formals)
        {
            return None;
        }
        let provisional = crate::symbol_resolver::ty_subst(
            signature.ret,
            &crate::symbol_resolver::GSigBinds::new(),
        );
        (matches!(actual, Ty::Error) || ty_mentions_param(actual, &open_formals))
            .then_some(signature)
            .or_else(|| (actual == provisional).then_some(signature))
    }

    /// A selected generic producer whose provisional result is only its declaration fallback.
    ///
    /// Ordinary calls retain their selected [`GenericSig`] directly. Constructors retain a stable
    /// constructor target instead, so rebuild their classifier-owned result template through
    /// [`Self::expected_type_callable_signature`]. When the checked probe is exactly that template's
    /// fallback substitution (`TypeToken<Any?>` for `TypeToken<T>`), it is not real argument
    /// evidence: an enclosing selected parameter may still complete `T`. Keeping that probe out of
    /// the outer call's first constraint round lets independent sibling arguments bind the outer
    /// formal before the constructor is rechecked against its exact parameter type.
    pub(super) fn unbound_contextual_result_signature(
        &self,
        expression: ExprId,
    ) -> Option<GenericSig> {
        if let Some(signature) = self.unbound_call_result_signature(expression) {
            return Some(signature.clone());
        }
        if !self.resolved_constructors.contains_key(&expression) {
            return None;
        }
        let signature = self.expected_type_callable_signature(expression)?;
        let actual = self.expr_types[expression.0 as usize];
        // Selected constructor parameters are declaration-owned shapes. A lexically fixed formal
        // is renamed in the result template, but still marks this call's input evidence.
        let declared_formals = self
            .resolved_constructors
            .get(&expression)
            .and_then(|constructor| self.resolved_type_name(constructor.owner()))
            .map(|classifier| classifier.type_params().clone())
            .unwrap_or_default();
        let shape_mentions_result_formal = |shape: Ty| {
            signature
                .formals
                .iter()
                .chain(&declared_formals)
                .any(|formal| ty_mentions_param(shape, std::slice::from_ref(formal)))
        };
        let has_input_evidence = match self.resolved_constructors.get(&expression)? {
            ResolvedConstructor::Source {
                params,
                argument_slots,
                ..
            } => argument_slots.iter().any(|&slot| {
                params
                    .get(slot)
                    .copied()
                    .is_some_and(shape_mentions_result_formal)
            }),
            ResolvedConstructor::Plain { member, args, .. } => {
                member.generic_sig.as_ref().is_some_and(|generic| {
                    generic
                        .params
                        .iter()
                        .take(args.len())
                        .copied()
                        .any(shape_mentions_result_formal)
                })
            }
            ResolvedConstructor::PlainSlots { member, slots, .. } => {
                member.generic_sig.as_ref().is_some_and(|generic| {
                    slots.iter().enumerate().any(|(parameter, argument)| {
                        argument.is_some()
                            && generic
                                .params
                                .get(parameter)
                                .copied()
                                .is_some_and(shape_mentions_result_formal)
                    })
                })
            }
            ResolvedConstructor::Synthetic { ctor, args, .. } => ctor
                .declaration
                .generic_sig
                .as_ref()
                .is_some_and(|generic| {
                    generic
                        .params
                        .iter()
                        .take(args.len())
                        .copied()
                        .any(shape_mentions_result_formal)
                }),
        };
        fn is_declaration_fallback(symbolic: Ty, actual: Ty, formals: &[String]) -> bool {
            match (symbolic, actual) {
                (Ty::TyParam(formal, bound), actual)
                    if formals.iter().any(|declared| declared == formal) =>
                {
                    // An alias formal (`E` of `typealias HashSet<E> = java.util.HashSet<E>`) is
                    // bounded by `Any?`; the Java target completes the same position from its own
                    // flexible bound `Any!`. Both are the declaration fallback.
                    actual == *bound
                        || matches!(actual, Ty::PlatformNullable(inner) if Ty::nullable(*inner) == *bound)
                }
                (Ty::Obj(symbolic_name, symbolic_args), Ty::Obj(actual_name, actual_args)) => {
                    symbolic_name == actual_name
                        && (actual_args.is_empty()
                            && symbolic_args.iter().any(|&argument| {
                                formals.iter().any(|formal| {
                                    ty_mentions_param(argument, std::slice::from_ref(formal))
                                })
                            })
                            || symbolic_args.len() == actual_args.len()
                                && symbolic_args.iter().zip(actual_args.iter()).all(
                                    |(&symbolic, &actual)| {
                                        is_declaration_fallback(symbolic, actual, formals)
                                    },
                                ))
                }
                (Ty::Nullable(symbolic), Ty::Nullable(actual))
                | (Ty::PlatformNullable(symbolic), Ty::PlatformNullable(actual))
                | (Ty::InProjection(symbolic), Ty::InProjection(actual))
                | (Ty::OutProjection(symbolic), Ty::OutProjection(actual))
                | (Ty::StarProjection(symbolic), Ty::StarProjection(actual)) => {
                    is_declaration_fallback(*symbolic, *actual, formals)
                }
                _ => symbolic == actual,
            }
        }
        let declaration_fallback = !has_input_evidence
            && is_declaration_fallback(signature.ret, actual, &signature.formals);
        crate::trace_compiler!(
            "expected_call",
            "unbound constructor expression={expression:?} actual={actual:?} input_evidence={has_input_evidence} declaration_fallback={declaration_fallback} signature={:?}",
            (&signature.formals, signature.ret),
        );
        (actual == Ty::Error
            || ty_mentions_param(actual, &signature.formals)
            || declaration_fallback)
            .then_some(signature)
    }
}
