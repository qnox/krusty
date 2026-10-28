//! The restartable group kotlinc's `ComposableFunctionBodyTransformer` gives a skippable composable
//! function, and the composer arguments `ComposerParamTransformer` passes at its composable calls.
//!
//! A composable function gains a `$composer: Composer?` and a `$changed: Int` parameter, and its
//! body becomes
//!
//! ```text
//! $composer = $composer.startRestartGroup(key)
//! sourceInformation($composer, "C(A)…")
//! if ($composer.shouldExecute($changed != 0, $changed and 1)) {
//!     if (isTraceInProgress()) traceEventStart(key, $changed, -1, "pkg.A (A.kt:4)")
//!     … the source statements …
//!     if (isTraceInProgress()) traceEventEnd()
//! } else {
//!     $composer.skipToGroupEnd()
//! }
//! $composer.endRestartGroup()?.updateScope { $composer, $force ->
//!     A($composer, updateChangedFlags($changed or 1))
//! }
//! ```
//!
//! The IR is built in the shape common lowering gives the same statements written in source, so
//! every target realizes it exactly as it realizes them.

use crate::ir::{
    Callee, ExprId, FunId, IrBinOp, IrConst, IrExpr, IrFile, IrFunction, IrParameterIdentity,
    IrTypeOp,
};
use crate::types::{EqualityMode, Ty};

use super::runtime;
use super::source_facts::{function_body, RestartableFunction};

/// The value slots of a transformed static function's two generated parameters.
const COMPOSER: u32 = 0;
const CHANGED: u32 = 1;

pub(super) fn transform(ir: &mut IrFile) {
    let Some(facts) = ir.compose.take() else {
        return;
    };
    for function in &facts.functions {
        add_composer_parameters(ir, function.function);
    }
    for function in &facts.functions {
        for &call in &function.calls {
            pass_composer(ir, call);
        }
        restart_group(ir, function);
        annotate_key(ir, function);
    }
}

/// Append `$composer` and `$changed` to a parameterless static function, moving its locals past
/// them, and record that its physical signature is no longer its declared one.
fn add_composer_parameters(ir: &mut IrFile, function: FunId) {
    let declaration = &mut ir.functions[function as usize];
    declaration.params = vec![Ty::nullable(runtime::composer()), Ty::Int];
    declaration.param_checks = vec![None, None];
    let body = declaration.body.expect("a restartable function has a body");
    crate::ir::shift_value_indices(ir, body, 0, 2);
    let parameters = ir.fn_params.entry(function).or_default();
    parameters.identities = vec![
        IrParameterIdentity::producer_value("$composer"),
        IrParameterIdentity::producer_value("$changed"),
    ];
    ir.fn_param_declared_nullable
        .insert(function, vec![true, false]);
    ir.plugin_extended_signatures.insert(function);
}

/// Give a composable call the caller's composer and an all-uncertain `$changed`.
fn pass_composer(ir: &mut IrFile, call: ExprId) {
    let composer = ir.add_expr(IrExpr::GetValue(COMPOSER));
    let changed = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    // kotlinc builds both arguments without offsets, so the call's line begins at its dispatch.
    ir.mark_positionless(composer);
    ir.mark_positionless(changed);
    let IrExpr::Call { args, .. } = &mut ir.exprs[call as usize] else {
        unreachable!("a composable call is a call");
    };
    args.extend([composer, changed]);
}

fn restart_group(ir: &mut IrFile, facts: &RestartableFunction) {
    let body = function_body(ir, facts.function).expect("a restartable function has a block body");
    let function_line = facts.opening_line;
    let mut statements = Vec::new();

    let composer = ir.add_expr(IrExpr::GetValue(COMPOSER));
    let key = ir.add_expr(IrExpr::Const(IrConst::Int(facts.key)));
    let started = runtime::composer_call(ir, composer, runtime::START_RESTART_GROUP, vec![key]);
    let start = ir.add_expr(IrExpr::SetValue {
        var: COMPOSER,
        value: started,
    });
    mark_line(ir, start, function_line);
    statements.push(start);

    if let Some(information) = &facts.source_information {
        let composer = ir.add_expr(IrExpr::GetValue(COMPOSER));
        let text = ir.add_expr(IrExpr::Const(IrConst::String(information.as_str().into())));
        statements.push(runtime::static_call(
            ir,
            runtime::SOURCE_INFORMATION,
            vec![composer, text],
        ));
    }

    // `$composer.shouldExecute($changed != 0, $changed and 1)`
    let composer = ir.add_expr(IrExpr::GetValue(COMPOSER));
    let changed = ir.add_expr(IrExpr::GetValue(CHANGED));
    let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let any_changed = ir.add_expr(IrExpr::Equality {
        op: IrBinOp::Ne,
        mode: EqualityMode::Primitive,
        lhs: changed,
        rhs: zero,
    });
    let changed = ir.add_expr(IrExpr::GetValue(CHANGED));
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let force = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::BitAnd,
        lhs: changed,
        rhs: one,
    });
    let execute = runtime::composer_call(
        ir,
        composer,
        runtime::SHOULD_EXECUTE,
        vec![any_changed, force],
    );

    let mut executed = Vec::new();
    if let Some(trace) = &facts.trace {
        let key = ir.add_expr(IrExpr::Const(IrConst::Int(facts.key)));
        let changed = ir.add_expr(IrExpr::GetValue(CHANGED));
        let unused = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
        let text = ir.add_expr(IrExpr::Const(IrConst::String(trace.as_str().into())));
        let event = runtime::static_call(
            ir,
            runtime::TRACE_EVENT_START,
            vec![key, changed, unused, text],
        );
        executed.push(if_trace_in_progress(ir, event));
    }
    executed.extend(body.statements.iter().copied());
    if facts.trace.is_some() {
        let event = runtime::static_call(ir, runtime::TRACE_EVENT_END, Vec::new());
        executed.push(if_trace_in_progress(ir, event));
    }
    let executed = ir.add_expr(IrExpr::Block {
        stmts: executed,
        value: None,
    });
    let composer = ir.add_expr(IrExpr::GetValue(COMPOSER));
    let skip = runtime::composer_call(ir, composer, runtime::SKIP_TO_GROUP_END, Vec::new());
    mark_line(ir, skip, function_line);
    ir.expr_source_lines.insert(skip, function_line);
    let skipped = ir.add_expr(IrExpr::Block {
        stmts: vec![skip],
        value: None,
    });
    let branch = ir.add_expr(IrExpr::When {
        branches: vec![(Some(execute), executed), (None, skipped)],
    });
    ir.whens.exhaustive.insert(branch, Ty::Unit);
    mark_line(ir, branch, function_line);
    statements.push(branch);

    let end = update_scope(ir, facts);
    mark_line(ir, end, facts.closing_line);
    statements.push(end);

    ir.exprs[body.block as usize] = IrExpr::Block {
        stmts: statements,
        value: None,
    };
}

/// `if (isTraceInProgress()) event`
fn if_trace_in_progress(ir: &mut IrFile, event: ExprId) -> ExprId {
    let probe = runtime::static_call(ir, runtime::IS_TRACE_IN_PROGRESS, Vec::new());
    let then = ir.add_expr(IrExpr::Block {
        stmts: vec![event],
        value: None,
    });
    let otherwise = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let when = ir.add_expr(IrExpr::When {
        branches: vec![(Some(probe), then), (None, otherwise)],
    });
    ir.whens.exhaustive.insert(when, Ty::Unit);
    when
}

/// `$composer.endRestartGroup()?.updateScope { $composer, $force -> F($composer, …) }`, as common
/// lowering realizes a safe call: the receiver in a temporary, then a null-guarded selector.
fn update_scope(ir: &mut IrFile, facts: &RestartableFunction) -> ExprId {
    let composer = ir.add_expr(IrExpr::GetValue(COMPOSER));
    let ended = runtime::composer_call(ir, composer, runtime::END_RESTART_GROUP, Vec::new());
    let scope_ty = Ty::nullable(runtime::scope_update_scope());
    let temporary = next_value_slot(ir, facts.function);
    let variable = ir.add_expr(IrExpr::Variable {
        index: temporary,
        ty: scope_ty,
        init: Some(ended),
        named: false,
    });
    let read = ir.add_expr(IrExpr::GetValue(temporary));
    let receiver = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: read,
        type_operand: runtime::scope_update_scope(),
    });
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let tested = ir.add_expr(IrExpr::GetValue(temporary));
    let condition = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: tested,
        rhs: null,
    });
    let lambda = recompose_lambda(ir, facts.function);
    let update = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::realized_virtual(
            runtime::scope_update_scope_name(),
            "updateScope".to_string(),
            "(Lkotlin/jvm/functions/Function2;)V".to_string(),
            None,
            true,
        )),
        dispatch_receiver: Some(receiver),
        args: vec![lambda],
    });
    let null_result = ir.add_expr(IrExpr::UnitInstance);
    let guarded = ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), null_result), (None, update)],
    });
    ir.whens.exhaustive.insert(guarded, Ty::Unit);
    ir.logical_types.insert(guarded, Ty::Unit);
    ir.null_guards.insert(guarded);
    let block = ir.add_expr(IrExpr::Block {
        stmts: vec![variable],
        value: Some(guarded),
    });
    ir.safe_calls.insert(block);
    block
}

/// `{ $composer, $force -> F($composer, updateChangedFlags($changed or 1)) }`, capturing
/// `$changed`, lifted to a private static `F$lambda$0`.
fn recompose_lambda(ir: &mut IrFile, function: FunId) -> ExprId {
    // The implementation's values: the captured `$changed`, then `$composer` and `$force`.
    let changed = ir.add_expr(IrExpr::GetValue(0));
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let forced = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::BitOr,
        lhs: changed,
        rhs: one,
    });
    let flags = runtime::static_call(ir, runtime::UPDATE_CHANGED_FLAGS, vec![forced]);
    let composer = ir.add_expr(IrExpr::GetValue(1));
    let restart = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(function)),
        dispatch_receiver: None,
        args: vec![composer, flags],
    });
    let unit = ir.add_expr(IrExpr::UnitInstance);
    let result = ir.add_expr(IrExpr::Return(Some(unit)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![restart, result],
        value: None,
    });
    let name = format!("{}$lambda$0", ir.functions[function as usize].name);
    let implementation = ir.add_fun(IrFunction {
        name,
        params: vec![Ty::Int, runtime::composer(), Ty::Int],
        ret: Ty::obj("kotlin/Unit"),
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None, None, None],
    });
    ir.set_method_visibility(implementation, crate::types::Visibility::Private);
    // kotlinc writes the recompose lambda's locals but no line of its own.
    let parameter_identities = vec![
        IrParameterIdentity::captured_value(
            Some("$changed".to_string()),
            0,
            crate::ir::IrValueCapture {
                declaration: crate::ir::IrCapturedDeclaration::Parameter,
                capturer: crate::ir::IrCapturingCallable::Lambda,
            },
        ),
        IrParameterIdentity::producer_value("$composer"),
        IrParameterIdentity::producer_value("$force"),
    ];
    ir.publish_generated_facade_function(crate::ir::IrGeneratedFunctionPublication {
        function: implementation,
        parameter_identities,
        metadata: None,
        debug: crate::ir::IrGeneratedDeclarationDebug::LocalsOnly,
    });
    let captured = ir.add_expr(IrExpr::GetValue(CHANGED));
    let lambda = ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 2,
        captures: vec![captured],
        sam: None,
        inline_body: None,
    });
    ir.note_synthesized_lambda(lambda);
    lambda
}

/// `@FunctionKeyMeta(key, startOffset, endOffset)`, which tooling maps a group key back to its
/// declaration with.
fn annotate_key(ir: &mut IrFile, facts: &RestartableFunction) {
    let Some(annotation) = facts.key_meta.then(|| runtime::function_key_meta(facts)) else {
        return;
    };
    ir.function_annotations
        .entry(facts.function)
        .or_default()
        .push(annotation);
}

/// The first value slot past every one the function's body uses.
fn next_value_slot(ir: &IrFile, function: FunId) -> u32 {
    let declaration = &ir.functions[function as usize];
    let mut next = declaration.params.len() as u32;
    let mut pending = declaration.body.into_iter().collect::<Vec<_>>();
    while let Some(expression) = pending.pop() {
        match &ir.exprs[expression as usize] {
            IrExpr::Variable { index, .. } => next = next.max(index + 1),
            IrExpr::Try { catches, .. } => {
                next = catches
                    .iter()
                    .fold(next, |next, catch| next.max(catch.var + 1));
            }
            IrExpr::Lambda { captures, .. } => {
                pending.extend(captures.iter().copied());
                continue;
            }
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    next
}

fn mark_line(ir: &mut IrFile, expression: ExprId, line: u32) {
    if line != 0 {
        ir.expr_lines.insert(expression, line);
    }
}
