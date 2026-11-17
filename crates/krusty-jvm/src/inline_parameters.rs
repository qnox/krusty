//! Which operands of a classpath inline call are inline parameters (kotlinc's
//! `InlineUtil.isInlineParameter`): a parameter whose declared type is a non-null function type
//! and that did not write `noinline`. A literal lambda passed anywhere else is an ordinary value,
//! even when the call's type arguments make the parameter a function type there (`with({ "" })
//! { … }` passes the lambda as the receiver `T`).

use krusty::ir::{ExprId, IrFile};
use krusty::types::{InlineParameterModifier, SemanticCallRole};

/// Whether operand `parameter` of the realized inline call `call` is an inline parameter, from the
/// declaration's published modifiers and declared parameter types. `None` when the call published
/// either fact for no such operand.
pub(crate) fn is_inline_parameter(ir: &IrFile, call: ExprId, parameter: usize) -> Option<bool> {
    let modifier = *ir.call_inline_modifiers.get(&call)?.get(parameter)?;
    let declared = *ir.call_declared_params.get(&call)?.get(parameter)?;
    Some(modifier != InlineParameterModifier::Noinline && declared.is_fun())
}

/// Whether operand `parameter` of the realized inline call `call` is a `crossinline` parameter.
/// `None` when the call published no modifier for that operand.
fn is_crossinline_parameter(ir: &IrFile, call: ExprId, parameter: usize) -> Option<bool> {
    let modifier = *ir.call_inline_modifiers.get(&call)?.get(parameter)?;
    Some(modifier == InlineParameterModifier::Crossinline)
}

/// Whether the literal for operand `parameter` of the realized inline call `call` is a
/// `crossinline` literal whose body the callee may move elsewhere, so kotlinc reaches the caller's
/// declarations from it as from outside the caller's class. A coroutine intrinsic
/// (`suspendCoroutine`) runs its block in place: its lambda keeps the caller's scope, as
/// kotlinc's `IrInlineScopeResolver` keeps it for `isBuiltInSuspendCoroutine`. `None` when the
/// call published no modifier for that operand.
pub(crate) fn crossinline_literal_moves(
    ir: &IrFile,
    call: ExprId,
    parameter: usize,
) -> Option<bool> {
    Some(is_crossinline_parameter(ir, call, parameter)? && moves_crossinline_literals(ir, call))
}

/// Whether `call`'s callee may move its `crossinline` literals ([`crossinline_literal_moves`]).
fn moves_crossinline_literals(ir: &IrFile, call: ExprId) -> bool {
    ir.semantic_call_roles.get(&call) != Some(&SemanticCallRole::KotlinSuspendCoroutine)
}

/// Whether operand `parameter` of the realized inline call `call` is an inline parameter of a
/// suspend function type, whose function value, when the call does not inline it, the inlined
/// code invokes as a suspension point. `None` when the call published either fact for no such
/// operand.
pub(crate) fn is_inline_suspend_parameter(
    ir: &IrFile,
    call: ExprId,
    parameter: usize,
) -> Option<bool> {
    let declared = *ir.call_declared_params.get(&call)?.get(parameter)?;
    let suspend = matches!(declared.non_null().kind(), krusty::types::TyKind::Fun(signature) if signature.suspend);
    Some(is_inline_parameter(ir, call, parameter)? && suspend)
}

/// The `crossinline` literal bodies a walk over common IR has met, so it can tell which code exports
/// its private access ([`crossinline_literal_bodies`]).
#[derive(Default)]
pub(crate) struct CrossinlineBodies(krusty::fx_hash::HashSet<ExprId>);

impl CrossinlineBodies {
    /// The children a walk visits under `expression`, each with whether its code exports private
    /// access: what `expression`'s own code does, or, for the body of one of its `crossinline`
    /// literals or the code an expansion spliced from such a body, always. A callable reference placed as a literal inlines its template, which is
    /// no child of the reference: where that template's code exports private access (the
    /// reference is itself a `crossinline` literal, or it stands in code that exports), the walk
    /// meets the template where the reference stands.
    pub(crate) fn children(
        &mut self,
        ir: &IrFile,
        expression: ExprId,
        export_private: bool,
    ) -> Vec<(ExprId, bool)> {
        self.0.extend(crossinline_literal_bodies(ir, expression));
        let mut children = Vec::new();
        krusty::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        if let Some((_, _, template, _)) = ir.callable_reference_inline_template(expression) {
            if export_private || self.0.contains(&template) {
                children.push(template);
            }
        }
        children
            .into_iter()
            .map(|child| {
                let crossinline = self.0.contains(&child) || ir.is_crossinline_body(child);
                (child, export_private || crossinline)
            })
            .collect()
    }
}

/// The bodies of the literals `call` passes to its `crossinline` parameters: a lambda's inline
/// body, or the template of a callable reference placed as that lambda. The callee may move such a
/// body into an object it creates, a class of its own, so kotlinc's `SyntheticAccessorLowering`
/// reaches every private declaration from it through a synthetic accessor, even where the body is
/// inlined into the class that declares it.
fn crossinline_literal_bodies(ir: &IrFile, call: ExprId) -> Vec<ExprId> {
    let Some(modifiers) = ir
        .call_inline_modifiers
        .get(&call)
        .filter(|_| moves_crossinline_literals(ir, call))
    else {
        return Vec::new();
    };
    let args: Vec<Option<ExprId>> = match ir.expr(call) {
        krusty::ir::IrExpr::Call { args, .. } => args.iter().copied().map(Some).collect(),
        krusty::ir::IrExpr::MethodCall { args, .. } => args.clone(),
        _ => return Vec::new(),
    };
    args.into_iter()
        .zip(modifiers.iter())
        .filter(|(_, &modifier)| modifier == InlineParameterModifier::Crossinline)
        .filter_map(|(argument, _)| literal_body(ir, argument?))
        .collect()
}

/// The body a literal argument inlines: a lambda's inline body, or a callable reference's template.
fn literal_body(ir: &IrFile, argument: ExprId) -> Option<ExprId> {
    match ir.expr(argument) {
        krusty::ir::IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } => Some(*body),
        krusty::ir::IrExpr::CallableReference(_) => ir
            .callable_reference_inline_template(argument)
            .map(|(_, _, body, _)| body),
        _ => None,
    }
}
