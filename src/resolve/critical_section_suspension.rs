//! kotlinc's JVM `FirJvmSuspensionPointInsideMutexLockChecker`: a suspension point inside the
//! block of `kotlin.synchronized`, or anywhere inside a `kotlin.concurrent.withLock` call, is an
//! error (`SUSPENSION_POINT_INSIDE_CRITICAL_SECTION`) when a suspend function or a suspend lambda
//! encloses it. A coroutine can resume on another thread, and the monitor or lock must be
//! released by the thread that acquired it.
//!
//! The rule walks a suspend call's enclosing elements outward, as kotlinc's checker does. A
//! suspend function, or a lambda passed to a suspend function type parameter, ends the walk: the
//! call suspends that body. A lambda typed as a suspend function only by an expected type that is
//! not a call's parameter (`val f: suspend () -> Unit = { … }`) does not. An anonymous function is
//! remembered as the argument the next enclosing call receives, so the `synchronized` call it is
//! the `block` of, or any enclosing `withLock` call, marks the critical section. A call
//! inside a lambda passed to another call in `synchronized(lock)`'s own arguments is not in its
//! block. The enclosing calls are resolved only once their statement is, so a suspend call is
//! recorded with its enclosing elements and judged when its outermost statement completes.

use super::*;

/// The suspend calls of the statements being checked, with the enclosing elements each was
/// written in.
#[derive(Default)]
pub(super) struct SuspensionPoints {
    /// Every function body being checked, innermost last: the expression depth its body starts
    /// at and whether it is a suspend function.
    functions: Vec<(usize, bool)>,
    pending: Vec<PendingSuspensionPoint>,
    /// A body is checked again on inference revisits; each call reports once.
    reported: crate::fx_hash::HashSet<ExprId>,
}

struct PendingSuspensionPoint {
    call: ExprId,
    /// The enclosing elements, outermost first.
    enclosing: Vec<Enclosing>,
}

#[derive(Clone, Copy)]
enum Enclosing {
    Expression(ExprId),
    Function { suspend: bool },
}

impl SuspensionPoints {
    /// Enter the body of named function `function`; the expressions checked from now on are
    /// written inside it.
    pub(super) fn enter_function(&mut self, expression_depth: usize, function: &FunDecl) {
        self.functions
            .push((expression_depth, function.is_suspend()));
    }

    pub(super) fn leave_function(&mut self) {
        self.functions.pop();
    }
}

impl Checker<'_> {
    /// Record `expression` when it calls a suspend function on a platform with critical sections.
    /// Its frame is the innermost of the expressions being checked.
    pub(super) fn record_suspension_point(&mut self, expression: ExprId) {
        if self.discover_anonymous_captures
            || !self.libraries.rejects_suspension_in_critical_sections()
            || !matches!(
                self.file.expr(expression),
                Expr::Call { .. } | Expr::SafeCall { args: Some(_), .. }
            )
            || !self
                .resolved_calls
                .get(&expression)
                .is_some_and(ResolvedCall::suspends)
        {
            return;
        }
        let frames = self
            .expectation_frames
            .iter()
            .map(|frame| frame.expression());
        let ancestors = frames.collect::<Vec<_>>();
        let ancestors = &ancestors[..ancestors.len().saturating_sub(1)];
        let points = &self.suspension_points;
        let mut functions = points.functions.iter().peekable();
        let mut enclosing = Vec::with_capacity(ancestors.len() + points.functions.len());
        for (depth, expression) in ancestors.iter().enumerate() {
            while let Some(&(_, suspend)) = functions.next_if(|(floor, _)| *floor <= depth) {
                enclosing.push(Enclosing::Function { suspend });
            }
            // A lambda is not an expression frame of its own: its root body is.
            if let Some(lambda) = self.lambda_returns.active_lambda_of_body(*expression) {
                enclosing.push(Enclosing::Expression(lambda));
            }
            enclosing.push(Enclosing::Expression(*expression));
        }
        enclosing.extend(functions.map(|&(_, suspend)| Enclosing::Function { suspend }));
        if points
            .pending
            .iter()
            .any(|recorded| recorded.call == expression)
        {
            return;
        }
        self.suspension_points.pending.push(PendingSuspensionPoint {
            call: expression,
            enclosing,
        });
    }

    /// Judge the suspend calls of the statement that has just been checked.
    pub(super) fn report_suspension_points(&mut self) {
        for point in std::mem::take(&mut self.suspension_points.pending) {
            if !self
                .resolved_calls
                .get(&point.call)
                .is_some_and(ResolvedCall::suspends)
            {
                continue;
            }
            if !self.suspension_points.reported.contains(&point.call)
                && self.inside_critical_section(&point.enclosing)
            {
                self.suspension_points.reported.insert(point.call);
                self.report_suspension_point(point.call);
            }
        }
    }

    fn inside_critical_section(&self, enclosing: &[Enclosing]) -> bool {
        let mut argument = None;
        let mut locked = false;
        for element in enclosing.iter().rev() {
            match *element {
                Enclosing::Function { suspend: true } => return locked,
                Enclosing::Function { suspend: false } => {}
                // `suspend { … }` is kotlinc's call of `kotlin.suspend`, whose parameter is a
                // suspend function type.
                Enclosing::Expression(lambda) if self.file.suspend_lambdas.contains(&lambda.0) => {
                    return locked;
                }
                Enclosing::Expression(lambda)
                    if matches!(self.file.expr(lambda), Expr::Lambda { .. }) =>
                {
                    argument = Some(lambda);
                }
                Enclosing::Expression(call) => {
                    // A lambda passed to a suspend function type parameter is a suspend body.
                    if argument.is_some_and(|lambda| self.suspend_lambda_argument(call, lambda)) {
                        return locked;
                    }
                    locked |= self.resolved_calls.get(&call).is_some_and(|resolved| {
                        self.locks_critical_section(call, resolved, argument)
                    });
                }
            }
        }
        false
    }

    /// Whether `lambda` is an argument of `call` and typed as a suspend function.
    fn suspend_lambda_argument(&self, call: ExprId, lambda: ExprId) -> bool {
        let arguments = match self.file.expr(call) {
            Expr::Call { args, .. }
            | Expr::SafeCall {
                args: Some(args), ..
            } => args,
            _ => return false,
        };
        arguments.contains(&lambda)
            && matches!(
                self.expr_types[lambda.0 as usize].kind(),
                TyKind::Fun(signature) if signature.suspend
            )
    }

    /// Whether `call` is a `withLock` call, or a `synchronized` call whose `block` is `argument`.
    fn locks_critical_section(
        &self,
        call: ExprId,
        resolved: &ResolvedCall,
        argument: Option<ExprId>,
    ) -> bool {
        let callable = match resolved {
            ResolvedCall::TopLevel(top_level) => &top_level.callable,
            ResolvedCall::Extension(extension) => &extension.callable,
            _ => return false,
        };
        let declared_in = |package: &str| {
            callable.declaration_owner
                == Some(crate::types::SemanticCallableOwner::Package(
                    crate::types::type_name(package),
                ))
        };
        if declared_in("kotlin/concurrent") && callable.name == "withLock" {
            return true;
        }
        let ResolvedCall::TopLevel(top_level) = resolved else {
            return false;
        };
        if !declared_in("kotlin") || callable.name != "synchronized" {
            return false;
        }
        let Some(argument) = argument else {
            return false;
        };
        self.resolved_call_arg_slots
            .get(&call)
            .and_then(|commitment| {
                commitment
                    .argument_bindings
                    .iter()
                    .find(|binding| binding.argument == argument)
            })
            .and_then(|binding| top_level.call_sig.param_names.get(binding.parameter))
            .is_some_and(|name| name == "block")
    }

    fn report_suspension_point(&mut self, call: ExprId) {
        let span = match self.file.expr(call) {
            Expr::Call { callee, .. } => match self.file.expr(*callee) {
                Expr::Member { name, .. } => self.member_name_span(*callee, name),
                _ => self.span(*callee),
            },
            Expr::SafeCall { name, .. } => self.member_name_span(call, name),
            _ => return,
        };
        let Some(name) = self
            .resolved_calls
            .get(&call)
            .and_then(|resolved| self.declared_callable_name(resolved))
        else {
            return;
        };
        self.diags.error_named(
            span,
            crate::diag::names::SUSPENSION_POINT_INSIDE_CRITICAL_SECTION,
            format!("The '{name}' suspension point is inside a critical section."),
        );
    }

    /// The name the selected declaration declares, which an import alias does not change.
    fn declared_callable_name(&self, resolved: &ResolvedCall) -> Option<String> {
        Some(match resolved {
            ResolvedCall::Member(member) => member.member.name.clone(),
            ResolvedCall::TopLevel(top_level) => top_level.callable.name.clone(),
            ResolvedCall::Companion(member) => member.name.clone(),
            ResolvedCall::Extension(extension) => extension.callable.name.clone(),
            ResolvedCall::MemberExtension { name, .. } => name.clone(),
            ResolvedCall::LocalFunction(local) => match self.file.stmt(local.stmt_id) {
                Stmt::LocalFun(function) => function.name.clone(),
                _ => return None,
            },
        })
    }
}
