//! Contract effects at call sites: which call declares a contract, and the smart casts a
//! `returns(…) implies …` effect proves.

use super::*;

/// What supplies one parameter of a selected call.
enum ContractArgument<'c> {
    Expression(ExprId),
    Context(&'c ResolvedContextArgument),
}

/// The context parameters of selected call `call`, one entry per leading context parameter:
/// the source selection chose from scope, or `None` for one written at the call site. A call
/// call that is not an ordinary selected callable has no mapping.
fn selected_context_arguments(
    call: &ResolvedCall,
) -> Option<Vec<Option<&ResolvedContextArgument>>> {
    Some(match call {
        ResolvedCall::Member(selected) => {
            selected.context_args.iter().map(Option::as_ref).collect()
        }
        ResolvedCall::TopLevel(selected) => {
            selected.context_args.iter().map(Option::as_ref).collect()
        }
        ResolvedCall::Extension(selected) => {
            selected.context_args.iter().map(Option::as_ref).collect()
        }
        ResolvedCall::MemberExtension { context_args, .. } => {
            context_args.iter().map(Option::as_ref).collect()
        }
        // A companion call commits no context arguments of its own.
        ResolvedCall::Companion(member) if member.context_count == 0 => Vec::new(),
        _ => return None,
    })
}

impl Checker<'_> {
    /// Whether checked call `e` selected the platform's erased contract-declaration intrinsic. The
    /// call is checked like any other first; this reads the declaration overload selection
    /// recorded for it, identified by its full signature.
    pub(super) fn selects_contract_intrinsic(&self, e: ExprId) -> bool {
        matches!(
            self.resolved_calls.get(&e),
            Some(ResolvedCall::TopLevel(call))
                if self.libraries.is_erased_contract_callable(&call.callable)
        )
    }

    /// The contract of the function a call resolved to: the Pass-1 contract or the metadata contract
    /// attached to the selected callable.
    pub(super) fn contract_for_call(
        &self,
        call: ExprId,
    ) -> Option<std::sync::Arc<crate::contracts::Contract>> {
        match self.resolved_calls.get(&call) {
            Some(ResolvedCall::Member(c)) => c.member.contract.clone(),
            Some(ResolvedCall::Companion(member)) => member.contract.clone(),
            Some(ResolvedCall::TopLevel(c)) => c.callable.contract.clone(),
            Some(ResolvedCall::Extension(c)) => c.callable.contract.clone(),
            _ => None,
        }
    }

    /// The argument a contract [`crate::contracts::ParamRef`] refers to at selected call `call`,
    /// from the mapping its selection committed. `Param(i)` indexes the declaration's logical
    /// parameters, leading context parameters first. A context parameter is supplied by the
    /// context source selection chose or, when written at the call site, by its argument; a value
    /// parameter by the argument its committed slot holds. A parameter the mapping leaves without
    /// an argument (a default, an empty vararg) has none.
    fn contract_argument(
        &self,
        call: ExprId,
        param: crate::contracts::ParamRef,
    ) -> Option<ContractArgument<'_>> {
        let crate::contracts::ParamRef::Param(index) = param else {
            let Expr::Call { callee, .. } = self.file.expr(call) else {
                return None;
            };
            return match self.file.expr(*callee) {
                Expr::Member { receiver, .. } => Some(ContractArgument::Expression(*receiver)),
                _ => None,
            };
        };
        let contexts = selected_context_arguments(self.resolved_calls.get(&call)?)?;
        let slots = self.resolved_call_arg_slots.get(&call);
        // Explicit context arguments follow the value arguments in the committed slots.
        let explicit_contexts = contexts.iter().filter(|source| source.is_none()).count();
        let ordinary = slots.map(|slots| slots.len().checked_sub(explicit_contexts));
        let slot = match contexts.get(index) {
            Some(Some(source)) => return Some(ContractArgument::Context(source)),
            Some(None) => {
                let explicit = contexts[..index]
                    .iter()
                    .filter(|source| source.is_none())
                    .count();
                ordinary??.checked_add(explicit)?
            }
            None => index - contexts.len(),
        };
        slots?
            .get(slot)
            .copied()
            .flatten()
            .map(ContractArgument::Expression)
    }

    /// The argument expression a contract parameter refers to at this call site, when an
    /// expression supplies it.
    pub(super) fn contract_arg_expr(
        &self,
        call: ExprId,
        param: crate::contracts::ParamRef,
    ) -> Option<ExprId> {
        match self.contract_argument(call, param)? {
            ContractArgument::Expression(expression) => Some(expression),
            ContractArgument::Context(_) => None,
        }
    }

    /// The stable ACCESS PATH a contract parameter refers to at this call site: its argument
    /// expression when that is a plain name or a property path (`requireNotNull(a.b)`,
    /// `a.p.isNullOrBlank()`), the context source selection chose for a context parameter
    /// supplied implicitly (`with("O") { validate1() }` → `this`), or the extension receiver
    /// the call selected when the source writes no receiver (`ready() && this.length`).
    pub(super) fn contract_stable_arg_path(
        &self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        param: crate::contracts::ParamRef,
    ) -> Option<NarrowPath> {
        if let Some(argument) = self.contract_argument(call, param) {
            return match argument {
                ContractArgument::Expression(expression) => self.expr_access_path(expression),
                ContractArgument::Context(source) => self.context_argument_path(scope, source),
            };
        }
        // `fun T?.ready()` called as `ready()` still selected the enclosing extension receiver.
        // The contract's `this@ready` is that receiver, the same value a later `this` denotes.
        if param == crate::contracts::ParamRef::Receiver {
            let identity = self.implicit_receiver_identities.get(&call).copied()?;
            return Some(NarrowPath::root_only(scope::PathRoot::Receiver(identity)));
        }
        None
    }

    /// Map a contract conclusion onto the call's actual arguments, producing `(path, Ty)`
    /// narrowings for stable access paths. Only sound forms narrow: `x != null`, `x is T`
    /// (positive), the boolean argument itself (recursed through the ordinary condition
    /// machinery), and `&&` compounds. `x == null`, `!is`, `||`, and constants yield nothing.
    /// A proof declined only because a capturing closure mutates the variable lands in `declined`
    /// exactly as for the ordinary condition machinery.
    pub(super) fn conclusion_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        conclusion: &crate::contracts::Condition,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
    ) {
        use crate::contracts::{Condition, ConditionType};
        match conclusion {
            Condition::IsNull {
                param,
                negated: true,
            } => {
                let Some(path) = self.contract_stable_arg_path(scope, call, *param) else {
                    return;
                };
                // Shared with the flow smart-cast paths (`stable_path_ty`): `this`,
                // `var`-rejection, and the nullable unwrap must not drift by condition shape.
                self.null_proof_or_decline(scope, &path, out, declined, self.span(call));
            }
            Condition::IsType {
                param,
                ty: ConditionType::Metadata(ty),
                negated: false,
            } => {
                // A contract decoded from `@Metadata`: the is-type is already semantic but may
                // mention the callee's type parameters (`value is R`) — substitute them with the
                // call's bindings (`Refinement<Any, String>.validate` → `String`).
                let Some(path) = self.contract_stable_arg_path(scope, call, *param) else {
                    return;
                };
                let tt = self.subst_contract_metadata_ty(call, *ty);
                if tt == Ty::Error || tt.is_ty_param() {
                    return;
                }
                let site = self.span(call);
                let Some(target) = self.type_test_target(
                    self.stable_path_ty(scope, &path, site),
                    tt,
                    tt.is_nullable(),
                    false,
                ) else {
                    return;
                };
                if !self.is_proof_stable_or_decline(scope, &path, target, declined, site) {
                    return;
                }
                out.push((path, target));
            }
            Condition::BoolParam { param, negated } => {
                if let Some(arg) = self.contract_arg_expr(call, *param) {
                    self.collect_condition_narrowings(scope, arg, !negated, out, declined);
                }
            }
            Condition::And(l, r) => {
                self.conclusion_narrowings(scope, call, l, out, declined);
                self.conclusion_narrowings(scope, call, r, out, declined);
            }
            _ => {}
        }
    }

    /// Substitute the callee's type parameters in a metadata-decoded contract type with the
    /// call's bindings: the checker's recorded type arguments first, then the receiver's type
    /// arguments unified against the declared receiver's type-parameter arguments (from the
    /// callable's metadata generic signature — `Refinement<Any, String>` against
    /// `Refinement<T, R>` binds `R = String`). Unbound parameters erase through `ty_subst`
    /// (to `Any`, a harmless no-narrow).
    pub(super) fn subst_contract_metadata_ty(&self, call: ExprId, ty: Ty) -> Ty {
        if !ty.is_ty_param() && ty.type_args().iter().all(|a| !a.is_ty_param()) {
            return ty;
        }
        let c = match self.resolved_calls.get(&call) {
            Some(ResolvedCall::TopLevel(c)) => &c.callable,
            Some(ResolvedCall::Extension(c)) => &c.callable,
            _ => return ty,
        };
        let gsig = c.generic_sig.as_ref();
        let formals: Vec<String> = gsig
            .map(|g| g.formals.clone())
            .or_else(|| {
                c.signature
                    .as_deref()
                    .map(|s| self.libraries.signature_formal_names(s))
            })
            .unwrap_or_default();
        if formals.is_empty() {
            return ty;
        }
        let mut binds: std::collections::HashMap<String, Ty> = std::collections::HashMap::new();
        if let Some(targs) = self.resolved_call_type_args.get(&call) {
            for (name, t) in formals.iter().zip(targs.iter()) {
                if let Some(t) = t {
                    binds.insert(name.clone(), *t);
                }
            }
        }
        if binds.is_empty() {
            let recv = self
                .contract_arg_expr(call, crate::contracts::ParamRef::Receiver)
                .map(|e| self.expr_types[e.0 as usize]);
            if let Some(Ty::Obj(_, actual_args)) = recv.map(|t| t.non_null()) {
                // Unify the DECLARED receiver's type-parameter arguments with the actual
                // receiver's type arguments (`Refinement<T, R>` on `Refinement<Any, String>`).
                if let Some(Ty::Obj(_, declared_args)) = gsig.and_then(|g| g.receiver) {
                    for (d, a) in declared_args.iter().zip(actual_args.iter()) {
                        if let Some(name) = d.ty_param_name() {
                            binds.entry(name.to_string()).or_insert(*a);
                        }
                    }
                } else if formals.len() == actual_args.len() {
                    // No declared receiver shape: pair the formals positionally (exact count).
                    for (name, t) in formals.iter().zip(actual_args.iter()) {
                        binds.insert(name.clone(), *t);
                    }
                }
            }
        }
        if binds.is_empty() {
            ty
        } else {
            crate::symbol_resolver::ty_subst(ty, &binds)
        }
    }

    /// Smart casts from the resolved callable's contract at a CONDITION position:
    /// `if (r.isErr())` with `returns(true) implies (this@isErr is Err)` narrows per the
    /// conclusion when the call's truth value matches the effect's return constant. The contract
    /// is decoded from same-file source or from `@Metadata` (`!s.isNullOrBlank()` narrows `s`
    /// through the deserialized kotlin.text contract — no per-function special cases).
    /// `nn(x) != null` likewise applies `returnsNotNull() implies …`.
    pub(super) fn contract_condition_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        truth: bool,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
    ) {
        use crate::contracts::ReturnsValue;
        // A Boolean call proves the effect for its own value; a call compared with `null` proves
        // `returnsNotNull()` when it is not null and `returns(null)` when it is.
        let (call, result) = match self.file.expr(cond) {
            Expr::Binary {
                op: op @ (BinOp::Eq | BinOp::Ne),
                lhs,
                rhs,
                ..
            } => {
                let call = match (self.file.expr(*lhs), self.file.expr(*rhs)) {
                    (_, Expr::NullLit) => *lhs,
                    (Expr::NullLit, _) => *rhs,
                    _ => return,
                };
                let not_null = (*op == BinOp::Ne) == truth;
                (
                    call,
                    if not_null {
                        ReturnsValue::NotNull
                    } else {
                        ReturnsValue::Null
                    },
                )
            }
            _ => (cond, ReturnsValue::Bool(truth)),
        };
        let Some(contract) = self.contract_for_call(call) else {
            return;
        };
        for effect in &contract.effects {
            let crate::contracts::Effect::ConditionalReturns {
                returns,
                conclusion,
            } = effect
            else {
                continue;
            };
            if *returns == result {
                self.conclusion_narrowings(scope, call, conclusion, out, declined);
            }
        }
    }
}
