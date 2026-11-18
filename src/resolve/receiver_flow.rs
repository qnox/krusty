//! Which value a flow proof is about, and how a later read finds that proof again.
//!
//! A narrowing proved by `if (ref != null)` has to be recovered when `ref` is read afterwards, and
//! the two sites must agree on WHICH value was narrowed. Both build the same [`NarrowPath`] from
//! what their checks selected: the lexical value's flow identity, the receiver-tower coordinate of
//! the receiver a property was read through, the selected property declarations. No step recovers
//! a binding, receiver or property from its spelling, so a proof about one receiver's property is
//! never consumed by a read of a same-named property of a nearer receiver.

use super::scope::{PathProperty, PathRoot};
use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// Applies the flow facts for the unqualified `this` value to the nearest receiver rung.
    ///
    /// The facts change that rung's semantic type, not its runtime identity: bare member lookup
    /// sees the narrowed type (`fun A?.bar() = if (this != null) foo() else ...`) while FIR keeps
    /// materializing the exact receiver depth selected by the tower.
    pub(super) fn narrow_current_implicit_receiver(
        &self,
        scope: &CheckerScope<'_>,
        receivers: &mut [ImplicitReceiver],
    ) {
        let Some(narrowed) = self.actual_this_narrow(scope) else {
            return;
        };
        let Some(current) = receivers.iter_mut().find(|receiver| receiver.current) else {
            return;
        };
        current.ty = narrowed;
        // An `is B` proof on a receiver of an incomparable declared class keeps both: bare
        // members of the declaration and of `B` stay visible (`check(this is B); field = x`).
        let context = crate::assignable::TyCtx::new();
        let declared = current.declared_ty;
        // A type-parameter receiver is judged by its bound: `this is B` on `M : A` with
        // `B : A` narrows `M` toward `B`, it does not introduce an unrelated class.
        let declared_class = declared
            .non_null()
            .ty_param_bound()
            .map_or(declared, Ty::non_null);
        let comparable = narrowed.non_null() == Ty::Nothing
            || crate::assignable::is_subtype(
                &context,
                self,
                declared_class.non_null(),
                narrowed.non_null(),
            )
            || crate::assignable::is_subtype(
                &context,
                self,
                narrowed.non_null(),
                declared_class.non_null(),
            );
        if !comparable {
            current.ty = Ty::intersection(&[narrowed, declared]);
        }
    }
}

impl Checker<'_> {
    /// The access path a checked expression denotes: the value its name read selected, followed by
    /// the member properties selected through plain (`.`) and safe (`?.`) reads. `None` for
    /// anything else (a call result, an indexed read, a temporary) — a proof on it says nothing
    /// about a later re-read.
    pub(super) fn expr_access_path(&self, e: ExprId) -> Option<NarrowPath> {
        match self.file.expr(e) {
            Expr::Name(_) => {
                if let Some(root) = self.read_flow_roots.get(&e) {
                    return Some(NarrowPath::root_only(root.clone()));
                }
                match self.expr_lowers.get(&e)? {
                    ExprLowering::MemberPropertyRead { .. } => {
                        let receiver = *self.implicit_receiver_identities.get(&e)?;
                        Some(
                            NarrowPath::root_only(PathRoot::Receiver(receiver))
                                .then(self.selected_path_property(e)?),
                        )
                    }
                    ExprLowering::LabeledThisInner | ExprLowering::LabeledThisDispatch => {
                        let receiver = *self.implicit_receiver_identities.get(&e)?;
                        Some(NarrowPath::root_only(PathRoot::Receiver(receiver)))
                    }
                    ExprLowering::TopLevelPropertyGet(access) => Some(NarrowPath::root_only(
                        PathRoot::TopLevel(self.top_level_path_property(&access.property)),
                    )),
                    _ => None,
                }
            }
            Expr::Member { receiver, .. }
            | Expr::SafeCall {
                receiver,
                args: None,
                ..
            } => Some(
                self.expr_access_path(*receiver)?
                    .then(self.selected_path_property(e)?),
            ),
            Expr::As {
                operand,
                nullable: false,
                ..
            } => self.expr_access_path(*operand),
            _ => None,
        }
    }

    /// The member property a checked read selected.
    fn selected_path_property(&self, read: ExprId) -> Option<PathProperty> {
        match self.expr_lowers.get(&read)? {
            ExprLowering::MemberPropertyRead {
                owner,
                name,
                declaration_ty,
                ..
            } => Some(PathProperty {
                owner: *owner,
                name: name.clone(),
                ty: *declaration_ty,
                stable: self.stable_property_reads.contains(&read),
            }),
            _ => None,
        }
    }

    /// A selected top-level property as a path root. A same-file `val` with its compiler-default
    /// backing-field getter is stable like a local `val`; cross-file, computed and delegated
    /// properties remain accessor reads.
    fn top_level_path_property(&self, property: &crate::libraries::PropertyInfo) -> PathProperty {
        let source_file = property.source_key.map(|(file, _)| file).or_else(|| {
            let declaration = property.stable_declaration?;
            self.resolved_index?
                .declaration_anchor(declaration)
                .map(|anchor| anchor.source.raw())
        });
        PathProperty {
            owner: property.owner,
            name: property.name.clone(),
            ty: property.ty,
            stable: property.context_count == 0
                && source_file == Some(self.file_index)
                && property.read_stability == crate::libraries::PropertyReadStability::Stable,
        }
    }

    /// Commit a `this@label` read of `receiver` and return its narrowed flow type, if any. A proof
    /// recorded on that receiver's tower coordinate (`this@f != null`, `this != null` on the same
    /// receiver, or a contract that named the receiver its call selected) applies to the labeled
    /// read exactly as it applies to a bare `this`; a proof about any other receiver does not.
    pub(super) fn select_labeled_receiver(
        &mut self,
        scope: &CheckerScope<'_>,
        read: ExprId,
        mut receiver: ImplicitReceiver,
        innermost: bool,
    ) -> Option<Ty> {
        let path = NarrowPath::root_only(PathRoot::Receiver(receiver.identity));
        let narrows = |narrowed: &Ty| *narrowed != receiver.declared_ty && *narrowed != Ty::Error;
        let narrowed = self
            .lookup_path_narrowing(scope, &path)
            .filter(narrows)
            .or_else(|| {
                innermost
                    .then(|| self.actual_this_narrow(scope))
                    .flatten()
                    .filter(narrows)
            })
            .or_else(|| Some(receiver.ty).filter(narrows));
        if let Some(narrowed) = narrowed {
            self.selected_value_smartcasts.insert(read, narrowed);
            receiver.ty = narrowed;
        }
        // `this@label` names its receiver explicitly, so `@DslMarker` scope control never applies.
        receiver.dsl_shadowed = false;
        self.mark_implicit_receiver_selection(read, receiver);
        narrowed
    }

    /// The path of the receiver `this` currently denotes.
    pub(super) fn current_receiver_path(&self, scope: &CheckerScope<'_>) -> Option<NarrowPath> {
        let receiver = self.declared_implicit_receivers(scope).into_iter().next()?;
        Some(NarrowPath::root_only(PathRoot::Receiver(receiver.identity)))
    }

    /// The path of the lexical value `name` currently denotes.
    pub(super) fn visible_value_path(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<NarrowPath> {
        let local = self.lookup(scope, name)?;
        Some(NarrowPath::root_only(PathRoot::Value(local.flow_identity)))
    }

    /// The path of the value the checker selected to fill a context parameter: the exact lexical
    /// binding at its recorded shadow depth, or the current receiver.
    pub(super) fn context_argument_path(
        &self,
        scope: &CheckerScope<'_>,
        source: &ResolvedContextArgument,
    ) -> Option<NarrowPath> {
        match source {
            ResolvedContextArgument::Binding { flow_identity, .. } => {
                Some(NarrowPath::root_only(PathRoot::Value((*flow_identity)?)))
            }
            ResolvedContextArgument::ImplicitReceiver(selection) if selection.current => {
                self.current_receiver_path(scope)
            }
            ResolvedContextArgument::ImplicitReceiver(_) => None,
        }
    }

    /// The visible lexical binding carrying flow identity `identity`, with the name it is visible
    /// under. A same-named nearer declaration hides it.
    pub(super) fn visible_flow_value(
        &self,
        scope: &CheckerScope<'_>,
        identity: u32,
    ) -> Option<(String, Local)> {
        let mut found = None;
        scope.visit_bindings(Ns::Value, |name, binding| {
            if found.is_none() {
                if let Some(local) = binding
                    .value()
                    .filter(|local| local.flow_identity == identity)
                {
                    found = Some((name.to_string(), local));
                }
            }
        });
        let (name, local) = found?;
        (self.lookup(scope, &name)?.flow_identity == identity).then_some((name, local))
    }

    /// Apply one narrowing without the support gate: a root-only lexical path shadows the binding
    /// in the current scope (reads resolve to the narrowed `Local`); every other path is recorded
    /// in the current flow frame for the read-time hooks.
    pub(super) fn apply_narrowing_unchecked(
        &mut self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        ty: Ty,
    ) {
        if let (PathRoot::Value(identity), true) = (&path.root, path.segments.is_empty()) {
            if let Some((name, _)) = self.visible_flow_value(scope, *identity) {
                crate::trace_compiler!("smartcast", "root narrowing shadows lexical value {name}");
                self.declare_narrowing_shadow(scope, &name, ty);
            }
            return;
        }
        self.record_path_narrowing(scope, path.clone(), ty);
        // A member property of a class body is also bound in the lexical namespace, so its bare
        // read resolves through that binding. The proof reaches it when the binding carries this
        // very receiver and declaration.
        if let (PathRoot::Receiver(receiver), [property]) = (&path.root, path.segments.as_slice()) {
            let carries_property = self.lookup(scope, &property.name).is_some_and(|local| {
                matches!(
                    local.origin,
                    ReceiverFnValueOrigin::DispatchProperty {
                        owner,
                        receiver_identity,
                        ..
                    } if owner == property.owner && receiver_identity == *receiver
                )
            });
            if carries_property {
                self.declare_narrowing_shadow(scope, &property.name, ty);
            }
        }
    }

    /// The type a proof gives a member property read through an explicit receiver: the proof
    /// holds while the path's stable type is still the declared one.
    pub(super) fn path_narrowed_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        read: ExprId,
        receiver: ExprId,
        declared: Ty,
    ) -> Ty {
        let Some(path) = self.expr_access_path(read) else {
            return declared;
        };
        self.proven_read_ty(scope, &path, self.span(receiver), declared)
    }

    /// The type a proof gives a bare member property read through the implicit receiver `receiver`
    /// selected for it.
    pub(super) fn receiver_property_narrowed_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        read: ExprId,
        receiver: (usize, usize),
        declared: Ty,
    ) -> Ty {
        let Some(property) = self.selected_path_property(read) else {
            return declared;
        };
        let path = NarrowPath::root_only(PathRoot::Receiver(receiver)).then(property);
        self.proven_read_ty(scope, &path, self.span(read), declared)
    }

    fn proven_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        site: Span,
        declared: Ty,
    ) -> Ty {
        let Some(narrowed) = self.lookup_path_narrowing(scope, path) else {
            return declared;
        };
        let current = self.stable_path_ty(scope, path, site);
        let still_valid = current.is_some_and(|current| current.non_null() == declared.non_null());
        crate::trace_compiler!(
            "smartcast",
            "read path={path:?} declared={declared:?} narrowed={narrowed:?} current={current:?} valid={still_valid}",
        );
        if narrowed != declared && still_valid {
            narrowed
        } else {
            declared
        }
    }

    /// The nearest proof recorded for `path`. The path's identities already name one value, so
    /// the walk needs no binding or receiver boundary.
    pub(super) fn lookup_path_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Option<Ty> {
        scope.ancestors().find_map(|rung| rung.path_narrowing(path))
    }

    pub(super) fn top_level_property_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        property: &crate::libraries::PropertyInfo,
        site: Span,
    ) -> Ty {
        let declared = property.ty;
        let path =
            NarrowPath::root_only(PathRoot::TopLevel(self.top_level_path_property(property)));
        crate::trace_compiler!(
            "smartcast",
            "top-level read candidate path={path:?} declared={declared:?}",
        );
        let Some(narrowed) = self.lookup_path_narrowing(scope, &path) else {
            return declared;
        };
        let stable = self.stable_path_ty(scope, &path, site);
        crate::trace_compiler!(
            "smartcast",
            "top-level read path={path:?} declared={declared:?} narrowed={narrowed:?} stable={stable:?}",
        );
        if stable == Some(declared) {
            narrowed
        } else {
            declared
        }
    }

    /// All negative facts currently proved for one stable path. The path's identities name one
    /// value, so the walk needs no binding or receiver boundary.
    pub(super) fn lookup_flow_exclusions(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Vec<FlowExclusion> {
        let mut exclusions = Vec::new();
        for rung in scope.ancestors() {
            for exclusion in rung.exclusions(path) {
                if !exclusions.contains(&exclusion) {
                    exclusions.push(exclusion);
                }
            }
        }
        exclusions
    }

    /// All incomparable smart-cast constituents currently proved for one access path. Like the
    /// ordinary path-narrowing lookup, the path's identities bound the walk. The vector is
    /// body-flow state only: callers project one constituent for a concrete semantic operation, so
    /// no synthetic intersection identity can escape into FIR.
    pub(super) fn lookup_intersection_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Vec<Ty> {
        scope
            .ancestors()
            .map(|rung| rung.intersection_narrowing(path))
            .find(|types| !types.is_empty())
            .unwrap_or_default()
    }

    /// The intersection a stable-path argument is proved to have, when it has several
    /// constituents. Overload comparison treats the argument as that type, so a parameter of any
    /// one constituent admits it.
    pub(super) fn flow_intersection_argument(
        &self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
    ) -> Option<Ty> {
        let path = self.expr_access_path(argument)?;
        let constituents = self.lookup_intersection_narrowing(scope, &path);
        (constituents.len() > 1).then(|| Ty::intersection(&constituents))
    }
}

/// Flow a completing expression proved in one scope frame: narrowing shadows, property paths, and
/// straight-line assignment types. Branch joins keep only the facts present on every completing
/// frame; the call that evaluated the expression then installs that delta for the arguments that
/// run next.
#[derive(Clone)]
pub(super) struct CompletedFlow {
    shadows: Vec<(String, Ty)>,
    paths: Vec<(NarrowPath, Ty)>,
    locals: Vec<(String, Ty)>,
}

impl CompletedFlow {
    pub(super) fn capture(checker: &Checker<'_>, scope: &CheckerScope<'_>) -> Self {
        let mut shadows = Vec::new();
        if let Some(parent) = scope.enclosing() {
            scope.own_bindings(Ns::Value, |name, binding| {
                let Some(local) = binding.value() else {
                    return;
                };
                let Some(outer) = checker.lookup(parent, name) else {
                    return;
                };
                if outer.flow_identity == local.flow_identity && outer.ty != local.ty {
                    shadows.push((name.to_string(), local.ty));
                }
            });
        }
        let locals = scope
            .own_local_narrowings()
            .into_iter()
            .filter(|(name, _)| !scope.declared_here(name, Ns::Value))
            .collect();
        Self {
            shadows,
            paths: scope.own_path_narrowings(),
            locals,
        }
    }

    pub(super) fn publish(self, checker: &mut Checker<'_>, scope: &CheckerScope<'_>) {
        for (name, ty) in self.shadows {
            if let Some(path) = checker.visible_value_path(scope, &name) {
                checker.apply_narrowing_unchecked(scope, &path, ty);
            }
        }
        for (path, ty) in self.paths {
            checker.apply_narrowing_unchecked(scope, &path, ty);
        }
        for (name, ty) in self.locals {
            // This is a fact proved by a completed expression, not a source write. Actual
            // assignments already record their binding identity while they are checked; using the
            // write API here would make an enclosing catch discard a still-valid entry narrowing.
            scope.narrow_local(&name, Some(ty));
        }
    }

    /// Install the facts shared by every completing branch. An empty list (every branch leaves)
    /// installs nothing.
    pub(super) fn publish_common(
        checker: &mut Checker<'_>,
        scope: &CheckerScope<'_>,
        flows: &[CompletedFlow],
    ) {
        let Some(common) = Self::intersect(flows) else {
            return;
        };
        common.publish(checker, scope);
    }

    fn intersect(flows: &[CompletedFlow]) -> Option<CompletedFlow> {
        let (first, rest) = flows.split_first()?;
        let mut common = first.clone();
        for other in rest {
            common
                .shadows
                .retain(|(name, ty)| other.shadows.iter().any(|(n, t)| n == name && t == ty));
            common
                .paths
                .retain(|(path, ty)| other.paths.iter().any(|(p, t)| p == path && t == ty));
            common
                .locals
                .retain(|(name, ty)| other.locals.iter().any(|(n, t)| n == name && t == ty));
        }
        Some(common)
    }
}

/// The call being resolved as an arbitrary invoke, apart from the scope its arguments use.
struct ArbitraryInvoke<'a> {
    call: ExprId,
    callee: ExprId,
    args: &'a [ExprId],
    span: Span,
    expected: Option<Ty>,
}

impl Checker<'_> {
    /// Whether evaluating `expression` returns to the code that follows it.
    pub(super) fn completed_normally(&self, expression: ExprId) -> bool {
        !self.expr_diverges(expression) && !self.expression_terminates_function(expression)
    }

    /// A hard cast proved its operand for the rest of this scope. Safe casts (`as?`) do not: the
    /// operand may still have its original type when the cast fails.
    pub(super) fn record_completed_cast(
        &mut self,
        scope: &CheckerScope<'_>,
        operand: ExprId,
        ty: &TypeRef,
        expression: ExprId,
    ) {
        let Some(path) = self.expr_access_path(operand) else {
            return;
        };
        let Some(stable_ty) = self.stable_path_ty(scope, &path, self.span(expression)) else {
            return;
        };
        let Some(narrowed) = self.proven_narrowed_ty(scope, Some(stable_ty), ty) else {
            return;
        };
        // Replace the stable type only when the cast refines it. `x: R as Any` and
        // `a as MutableList<Any?>` keep the original type; `a: Any as String` narrows.
        // An unrelated cast written as its own statement still reaches later reads
        // through the block's cast walk.
        if self.receiver_is_assignable(narrowed, stable_ty)
            && !self.receiver_is_assignable(stable_ty, narrowed)
        {
            self.apply_narrowing_unchecked(scope, &path, narrowed);
        }
    }

    /// Check `receiver` once, in source order, and return the scope arguments of this call are
    /// typed under. Facts the receiver proved are on that scope, and — when the receiver returns —
    /// on `scope` as well, so a later argument of an enclosing call sees them.
    pub(super) fn check_receiver_before_arguments<'p>(
        &mut self,
        scope: &'p CheckerScope<'p>,
        receiver: ExprId,
    ) -> (Ty, CheckerScope<'p>) {
        let argument_scope = scope.child(ScopeKind::Block);
        let ty = self.expr(&argument_scope, receiver);
        if self.completed_normally(receiver) {
            CompletedFlow::capture(self, &argument_scope).publish(self, scope);
        }
        (ty, argument_scope)
    }

    /// Check a call. A member call types its receiver once, then its arguments, in one scope of
    /// its own, so the facts the receiver proved reach the arguments. When the call returns, the
    /// facts the receiver and the arguments proved belong to the code that follows it
    /// (`b.take(x!!)` proves `x` non-null afterwards), exactly as for any other call.
    pub(super) fn check_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        callee: ExprId,
        args: &[ExprId],
        span: Span,
        expected: Option<Ty>,
    ) -> Ty {
        if !matches!(self.file.expr(callee), Expr::Member { .. }) {
            return self.check_call_in_argument_scope(scope, call, callee, args, span, expected);
        }
        let argument_scope = scope.child(ScopeKind::Block);
        let ty =
            self.check_call_in_argument_scope(&argument_scope, call, callee, args, span, expected);
        if self.completed_normally(call) {
            CompletedFlow::capture(self, &argument_scope).publish(self, scope);
        }
        ty
    }

    /// A lambda literal whose parameter shape is supplied by the call's argument probes. An
    /// anonymous function declares that shape itself and is an ordinary callee.
    fn postponed_lambda_callee(&self, callee: ExprId) -> bool {
        matches!(self.file.expr(callee), Expr::Lambda { .. })
            && !self.file.anon_fun_lambdas.contains(&callee.0)
    }

    /// Invoke an arbitrary callee (`make()(x)`, `(a as String)(a)`). A postponed lambda, and a
    /// callee whose value is a callable reference under `when`/`if`/`try`/`?:`, is typed once
    /// after its argument probes: those probes are the function shape. Every other callee is
    /// typed once before its arguments, and those arguments see the flow that check produced.
    pub(super) fn check_arbitrary_callee(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        callee: ExprId,
        args: &[ExprId],
        span: Span,
        expected: Option<Ty>,
    ) -> Ty {
        let site = ArbitraryInvoke {
            call,
            callee,
            args,
            span,
            expected,
        };
        if self.postponed_lambda_callee(callee) || self.adapts_to_expected_function(callee) {
            return self.invoke_probed_callee(scope, site);
        }
        let (callee_ty, argument_scope) = self.check_receiver_before_arguments(scope, callee);
        self.finish_arbitrary_invoke(&argument_scope, site, callee_ty, None)
    }

    /// The callee's type is a callable reference, or a control-flow join of one. Its shape comes
    /// from the call's expected function type, so the arguments are probed first and the callee
    /// is still checked only once.
    fn adapts_to_expected_function(&self, expression: ExprId) -> bool {
        match self.file.expr(expression).clone() {
            Expr::CallableRef { .. } => true,
            Expr::When { arms, .. } => arms
                .iter()
                .any(|arm| self.adapts_to_expected_function(arm.body)),
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.adapts_to_expected_function(then_branch)
                    || else_branch.is_some_and(|branch| self.adapts_to_expected_function(branch))
            }
            Expr::Try { body, catches, .. } => {
                self.adapts_to_expected_function(body)
                    || catches
                        .iter()
                        .any(|clause| self.adapts_to_expected_function(clause.body))
            }
            Expr::Elvis { lhs, rhs } => {
                self.adapts_to_expected_function(lhs) || self.adapts_to_expected_function(rhs)
            }
            Expr::Block { trailing, .. } => {
                trailing.is_some_and(|trailing| self.adapts_to_expected_function(trailing))
            }
            _ => false,
        }
    }

    fn invoke_probed_callee(&mut self, scope: &CheckerScope<'_>, site: ArbitraryInvoke<'_>) -> Ty {
        let probes = site
            .args
            .iter()
            .map(|&argument| {
                self.lambda_probe_ty(argument).unwrap_or_else(|| {
                    if matches!(self.file.expr(argument), Expr::CallableRef { .. }) {
                        Ty::Error
                    } else {
                        self.expr(scope, argument)
                    }
                })
            })
            .collect::<Vec<_>>();
        // The surrounding call constrains the callee's return only when this expression itself
        // has an expected result. With no such constraint, let the callee infer its own result:
        // forcing `Any` here changes `({ "s" })()` from `String` to `Any` before the invocation
        // is selected.
        let callee_ty = match site.expected.filter(|ty| *ty != Ty::Error) {
            Some(result) => self.expr_expected(scope, site.callee, Ty::fun(probes.clone(), result)),
            None => self.expr(scope, site.callee),
        };
        self.finish_arbitrary_invoke(scope, site, callee_ty, Some(probes))
    }

    fn finish_arbitrary_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        site: ArbitraryInvoke<'_>,
        callee_ty: Ty,
        probes: Option<Vec<Ty>>,
    ) -> Ty {
        let params = self
            .expression_function_type(scope, site.callee, callee_ty)
            .and_then(|semantic| match semantic.kind() {
                TyKind::Fun(signature) if signature.params.len() == site.args.len() => {
                    Some(signature.params.clone())
                }
                _ => None,
            })
            .or_else(|| self.object_invoke_operator_parameter_shape(callee_ty, site.args.len()));
        let arg_tys = site
            .args
            .iter()
            .enumerate()
            .map(|(index, &argument)| {
                let probed = probes
                    .as_ref()
                    .and_then(|probes| probes.get(index))
                    .copied();
                let Some(expected_arg) = params
                    .as_ref()
                    .and_then(|parameters| parameters.get(index))
                    .copied()
                else {
                    return probed.unwrap_or_else(|| self.probe_or_check_argument(scope, argument));
                };
                if matches!(
                    self.file.expr(argument),
                    Expr::Lambda { .. } | Expr::CallableRef { .. }
                ) || self.call_result_can_bind_expected(argument, expected_arg)
                {
                    self.expr_expected(scope, argument, expected_arg)
                } else {
                    probed.unwrap_or_else(|| self.expr(scope, argument))
                }
            })
            .collect::<Vec<_>>();
        if let Some(ret) = self.record_invoke_or_report(
            scope,
            CallArgs {
                call: site.call,
                args: site.args,
                arg_tys: &arg_tys,
            },
            site.callee,
            callee_ty,
            site.span,
            CallResultConstraint::direct(site.expected),
        ) {
            return ret;
        }
        if callee_ty != Ty::Error {
            self.diags.error(site.span, "expression is not callable");
        }
        Ty::Error
    }

    /// The type an argument contributes before its callee's parameter type is known. A callable
    /// reference and a lambda literal stay postponed; every other argument is checked once.
    fn probe_or_check_argument(&mut self, scope: &CheckerScope<'_>, argument: ExprId) -> Ty {
        self.lambda_probe_ty(argument).unwrap_or_else(|| {
            if matches!(self.file.expr(argument), Expr::CallableRef { .. }) {
                Ty::Error
            } else {
                self.expr(scope, argument)
            }
        })
    }
}

/// The type a flow fact proved for one implicit receiver, named by that receiver's tower identity.
#[derive(Clone, Copy)]
pub(super) struct ReceiverNarrowing {
    pub(super) receiver: (usize, usize),
    pub(super) ty: Ty,
}

impl Checker<'_> {
    pub(super) fn with_this_narrow<R>(
        &mut self,
        narrow: Option<ReceiverNarrowing>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prev = self.this_narrow;
        if let Some(n) = narrow {
            self.this_narrow = Some(n);
        }
        let r = f(self);
        self.this_narrow = prev;
        r
    }

    /// Current smart-cast type of the implicit receiver.
    pub(super) fn effective_this_narrow(&self, scope: &CheckerScope<'_>) -> Option<Ty> {
        self.current_receiver_narrowing(scope).or_else(|| {
            self.lookup(scope, "this")
                .map(|local| self.local_narrowing(scope, "this").unwrap_or(local.ty))
        })
    }

    /// The type `if (this is B)` proved for the receiver `this` currently denotes. The proof belongs
    /// to the receiver it was made about, so it is absent inside a receiver lambda that pushed a
    /// nearer one.
    pub(super) fn current_receiver_narrowing(&self, scope: &CheckerScope<'_>) -> Option<Ty> {
        let narrowing = self.this_narrow?;
        let current = self.declared_implicit_receivers(scope).into_iter().next()?;
        (current.identity == narrowing.receiver).then_some(narrowing.ty)
    }

    pub(super) fn actual_this_narrow(&self, scope: &CheckerScope<'_>) -> Option<Ty> {
        self.current_receiver_narrowing(scope)
            .or_else(|| self.local_narrowing(scope, "this"))
            .or_else(|| self.lookup_path_narrowing(scope, &self.current_receiver_path(scope)?))
            .or_else(|| {
                let declared = scope.this_ty()?;
                let shadow = self.lookup(scope, "this")?.ty;
                (shadow != declared).then_some(shadow)
            })
    }

    /// The narrowed implicit-receiver type established by `if (this is B)` (`this !is B` in the
    /// else-branch): `this` is a subtype `B` of the declared receiver. `this` is immutable, so the
    /// narrowing is sound across nested scopes. Only a KNOWN reference subtype narrows (a primitive /
    /// nullable / unresolved target is left un-narrowed, so the file skips rather than miscompiles).
    pub(super) fn this_is_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        for_else: bool,
    ) -> Option<ReceiverNarrowing> {
        let Expr::Is {
            operand,
            ty,
            negated,
        } = self.file.expr(cond).clone()
        else {
            return None;
        };
        if negated != for_else {
            return None;
        }
        if !matches!(self.file.expr(operand), Expr::Name(n) if n == "this") {
            return None;
        }
        if ty.nullable() {
            return None;
        }
        let tt = self
            .resolved_type_tys
            .get(&(ty.span.lo, ty.span.hi))
            .copied()
            .unwrap_or_else(|| self.type_ref_ty_silent(scope, &ty));
        let runtime_tt =
            crate::symbol_resolver::classifier_callable_signature(&self.fed_source(), tt)
                .unwrap_or(tt);
        // Context-sensitive nested type syntax may omit the subclass's type arguments (`is Right`)
        // while the subject supplies them through its sealed/generic supertype (`Either<E, A>`).
        // The general smart-cast path already reconstructs `Right<A>` from that relation; the
        // implicit-`this` fast path must preserve the same applied target instead of replacing it
        // with raw `Right`, whose `value` property subsequently erases to `Any?`.
        let runtime_tt = self
            .current_receiver_path(scope)
            .and_then(|path| self.stable_path_ty(scope, &path, self.span(cond)))
            .filter(|_| ty.targs.is_empty())
            .map(|declared| {
                crate::symbol_resolver::apply_subtype_arguments_from_supertype(
                    &self.fed_source(),
                    runtime_tt,
                    declared,
                )
            })
            .unwrap_or(runtime_tt);
        let scope::PathRoot::Receiver(receiver) = self.current_receiver_path(scope)?.root else {
            return None;
        };
        runtime_tt.is_reference().then_some(ReceiverNarrowing {
            receiver,
            ty: runtime_tt,
        })
    }
}
