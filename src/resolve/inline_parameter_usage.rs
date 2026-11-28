//! Reads of an inline function's inlinable parameters (kotlinc's `USAGE_IS_NOT_INLINABLE`).
//!
//! A function-typed parameter of an `inline` function that is neither `noinline` nor nullable is
//! inlined at each call site, so the body may only invoke it (`s(x)`, `s.invoke(x)`) or pass it on
//! to an inlined parameter of another inline function. Every other read needs the parameter as a
//! value and is rejected: an argument of a non-inline function or of a `noinline` parameter, the
//! receiver of a safe call, `!!` or any other member or extension call, a stored value, a branch
//! result, and any read inside a lambda that does not invoke it. A `contract { … }` block names
//! the parameters without running, so its reads are not usages.
//!
//! The analysis runs once the body is checked, over the reads the checker bound to each parameter
//! and the calls it selected. A call whose selected parameter cannot be told apart is not reported.

use super::scope::PathRoot;
use super::*;
use crate::types::{InlineParameterModifier, TyKind};

/// One parameter the body may only invoke or inline.
struct InlinableParameter {
    name: String,
    ty: Ty,
}

impl Checker<'_> {
    /// Report every read of an inlinable parameter of `function` that is not an invocation or an
    /// inlined argument. `scope` is the function's parameter scope.
    pub(super) fn check_inline_parameter_usages(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) {
        if !function.is_inline() || self.signature_defaults_only {
            return;
        }
        let Some(body) = fun_body_expr(&function.body) else {
            return;
        };
        let parameters = function
            .params
            .iter()
            .skip(function.context_count)
            .filter(|parameter| {
                lambda_expectation::written_inline_modifier(parameter)
                    != InlineParameterModifier::Noinline
            })
            .filter_map(|parameter| {
                let local = self.lookup(scope, &parameter.name)?;
                self.is_inlinable_parameter_type(local.declared_ty)
                    .then(|| {
                        (
                            local.flow_identity,
                            InlinableParameter {
                                name: parameter.name.clone(),
                                ty: local.declared_ty,
                            },
                        )
                    })
            })
            .collect::<HashMap<_, _>>();
        if parameters.is_empty() {
            return;
        }
        let mut illegal = Vec::new();
        UsageAnalysis {
            checker: self,
            parameters: &parameters,
        }
        .child(body, &mut illegal);
        for read in illegal {
            if !self.inline_access.first_parameter_usage_report(read) {
                continue;
            }
            let Some(parameter) = analysis_parameter(self, &parameters, read) else {
                continue;
            };
            let message = format!(
                "Illegal usage of inline parameter '{}: {}'. Add 'noinline' modifier to the parameter declaration.",
                parameter.name,
                parameter.ty.source_name(),
            );
            self.diags.error_named(
                self.span(read),
                crate::diag::names::USAGE_IS_NOT_INLINABLE,
                message,
            );
        }
    }

    /// A non-null function type, written as such or as a function classifier.
    fn is_inlinable_parameter_type(&self, ty: Ty) -> bool {
        !ty.is_nullable()
            && matches!(
                self.declared_function_semantic_type(ty).kind(),
                TyKind::Fun(_)
            )
    }
}

fn analysis_parameter<'p>(
    checker: &Checker<'_>,
    parameters: &'p HashMap<u32, InlinableParameter>,
    read: ExprId,
) -> Option<&'p InlinableParameter> {
    match checker.read_flow_roots.get(&read) {
        Some(PathRoot::Value(identity)) => parameters.get(identity),
        _ => None,
    }
}

struct UsageAnalysis<'a, 'c> {
    checker: &'a Checker<'c>,
    parameters: &'a HashMap<u32, InlinableParameter>,
}

impl UsageAnalysis<'_, '_> {
    /// The parameter `expression` reads, when it is a bare read of one.
    fn parameter_read(&self, expression: ExprId) -> Option<&InlinableParameter> {
        if !matches!(self.checker.file.expr(expression), Expr::Name(_)) {
            return None;
        }
        analysis_parameter(self.checker, self.parameters, expression)
    }

    /// Walk `expression`, whose own position was already judged, collecting its illegal reads.
    fn expression(&self, expression: ExprId, illegal: &mut Vec<ExprId>) {
        // A `contract { … }` block describes the parameters; its body is never executed.
        if self.checker.selects_contract_intrinsic(expression) {
            return;
        }
        match self.checker.file.expr(expression) {
            Expr::Call { callee, args } => {
                self.callee(expression, *callee, illegal);
                self.arguments(expression, args, illegal);
                return;
            }
            // `receiver?.member(arguments)`: the receiver needs a value, the arguments are passed
            // to the selected member like any call's.
            Expr::SafeCall {
                receiver,
                args: Some(args),
                ..
            } => {
                self.child(*receiver, illegal);
                self.arguments(expression, args, illegal);
                return;
            }
            _ => {}
        }
        let mut expressions = Vec::new();
        let mut statements = Vec::new();
        self.checker.file.any_child_expr(
            expression,
            &mut |child| {
                expressions.push(child);
                false
            },
            &mut |statement| {
                statements.push(statement);
                false
            },
        );
        for statement in statements {
            self.checker.file.any_child_stmt(statement, &mut |child| {
                expressions.push(child);
                false
            });
        }
        for child in expressions {
            self.child(child, illegal);
        }
    }

    /// The value arguments of `call`: a parameter passed on is legal where the callee inlines it.
    fn arguments(&self, call: ExprId, arguments: &[ExprId], illegal: &mut Vec<ExprId>) {
        for &argument in arguments {
            if self.parameter_read(argument).is_some() {
                if self.inlined_argument(call, argument) == Some(false) {
                    illegal.push(argument);
                }
            } else {
                self.expression(argument, illegal);
            }
        }
    }

    /// A child in a position that needs its value: a parameter read there is illegal.
    fn child(&self, expression: ExprId, illegal: &mut Vec<ExprId>) {
        if self.parameter_read(expression).is_some() {
            illegal.push(expression);
        } else {
            self.expression(expression, illegal);
        }
    }

    /// The callee of `call`. Invoking the parameter, directly or through its own `invoke`, is the
    /// use an inlinable parameter exists for.
    fn callee(&self, call: ExprId, callee: ExprId, illegal: &mut Vec<ExprId>) {
        if self.parameter_read(callee).is_some() {
            return;
        }
        if let Expr::Member { receiver, .. } = self.checker.file.expr(callee) {
            if self.parameter_read(*receiver).is_some() {
                if !self.invokes_function_value(call) {
                    illegal.push(*receiver);
                }
                return;
            }
        }
        self.expression(callee, illegal);
    }

    /// Whether `call` invokes its receiver's function value: the selection is the function
    /// type's own `invoke`, which no source or provider declaration supplies.
    fn invokes_function_value(&self, call: ExprId) -> bool {
        !self.checker.resolved_calls.contains_key(&call)
            && matches!(
                self.checker.file.expr(call),
                Expr::Call { callee, .. }
                    if matches!(self.checker.file.expr(*callee), Expr::Member { name, .. } if name == "invoke")
            )
    }

    /// Whether the selected callee of `call` inlines `argument`: `Some(false)` for a non-inline
    /// callee or a `noinline` parameter, `None` when the selection does not say or failed.
    fn inlined_argument(&self, call: ExprId, argument: ExprId) -> Option<bool> {
        // A call whose callee failed to resolve has already been reported.
        if self.checker.expr_types[call.0 as usize] == Ty::Error {
            return None;
        }
        let resolved = self.checker.resolved_calls.get(&call)?;
        let (inline, modifiers) = resolved.inline_parameter_contract();
        if !inline {
            return Some(false);
        }
        let modifiers = modifiers?;
        let commitment = self.checker.resolved_call_arg_slots.get(&call)?;
        let parameter = commitment
            .argument_bindings
            .iter()
            .find(|binding| binding.argument == argument)
            .map(|binding| binding.parameter)
            .or_else(|| {
                commitment
                    .slots
                    .iter()
                    .position(|slot| *slot == Some(argument))
            })?;
        Some(modifiers.get(parameter) != Some(&InlineParameterModifier::Noinline))
    }
}

impl ResolvedCall {
    /// Whether the selected callable is inline, and its parameters' `noinline`/`crossinline`
    /// modifiers when the selection carries them.
    fn inline_parameter_contract(&self) -> (bool, Option<&[InlineParameterModifier]>) {
        match self {
            Self::Member(resolved) => (
                resolved.member.inline.can_inline(),
                Some(&resolved.member.call_sig.inline_modifiers),
            ),
            Self::TopLevel(call) => (
                call.callable.inline.can_inline(),
                Some(&call.call_sig.inline_modifiers),
            ),
            Self::Companion(member) => (
                member.inline.can_inline(),
                Some(&member.call_sig.inline_modifiers),
            ),
            Self::Extension(extension) => (extension.callable.inline.can_inline(), None),
            Self::MemberExtension { inline, .. } => (inline.can_inline(), None),
            Self::LocalFunction(call) => (call.sig.is_inline(), Some(&call.sig.inline_modifiers)),
        }
    }
}
