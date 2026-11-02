//! How kotlinc realizes a `suspend inline` function's body on the JVM.
//!
//! An inlining caller splices the body kotlinc's codegen wrote, suspension markers and all, and
//! its own coroutine transformation builds the machine over the result. A callable function keeps
//! that body in a private `name$$forInline` copy beside its own state machine. A function that is
//! only ever inlined (kotlinc's `isEffectivelyInlineOnly`) has no state machine: its one method is
//! that body.
//!
//! A suspend method of a class that captures a `crossinline` parameter of the inline function it is
//! written in, and reads that capture, is inlined in the same way: a call site that inlines the
//! function copies the class and inlines the lambda into the method (kotlinc's
//! `isSuspendCapturingCrossinline`). kotlinc keeps that method's marked body in a
//! `name$$forInline` copy too, which keeps the method's access and starts by constructing the
//! method's continuation between `InlineMarker.mark(4)` and `mark(5)`, so the copy's machine can be
//! built over it.

use crate::fx_hash::HashSet;
use crate::ir::{ClassId, ExprId, IrClass, IrExpr, IrFile};
use crate::jvm::suspend::TransformedMachine;
use crate::types::InlineExpansionMode;

/// Whether `fid` is a `suspend inline` function only ever inlined: a reified one, whose body
/// needs its call site's types, or a private one. Its method keeps the marked body and no
/// continuation class is written for it.
pub(crate) fn keeps_marked_body(ir: &IrFile, fid: u32) -> bool {
    is_suspend_inline(ir, fid)
        && (crate::jvm::ir_emit::is_reifiable(ir, fid) || ir.method_visibility(fid).is_private())
}

/// Why a suspend function keeps its marked body in a `name$$forInline` copy beside its machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForInlineCopy {
    /// A callable `suspend inline` function: the copy is private.
    InlineFunction,
    /// A suspend method that reads a `crossinline` capture of its class: the copy keeps the
    /// method's access and constructs the continuation first.
    CrossinlineCapture,
}

/// Whether `fid` is written as its state machine with its marked body in a `name$$forInline` copy,
/// and why.
pub(crate) fn for_inline_copy(ir: &IrFile, fid: u32) -> Option<ForInlineCopy> {
    if is_suspend_inline(ir, fid) {
        return (!keeps_marked_body(ir, fid)).then_some(ForInlineCopy::InlineFunction);
    }
    reads_crossinline_capture(ir, fid).then_some(ForInlineCopy::CrossinlineCapture)
}

/// Whether continuation class `class` belongs to a method with a `$$forInline` copy that reads a
/// `crossinline` capture. The copy constructs it, so a call site in another class that inlines the
/// copy does too: kotlinc writes it public, and even when the method's own body needs no machine.
pub(crate) fn constructed_by_method_copy(ir: &IrFile, class: ClassId) -> bool {
    ir.jvm_continuation_functions
        .get(&class)
        .is_some_and(|&function| {
            for_inline_copy(ir, function) == Some(ForInlineCopy::CrossinlineCapture)
        })
}

/// Whether `fid` is a suspend method that reads one of its class's `crossinline` captures.
fn reads_crossinline_capture(ir: &IrFile, fid: u32) -> bool {
    if !ir.suspend_funs.contains(&fid) || ir.inline_fns.contains(&fid) {
        return false;
    }
    let function = &ir.functions[fid as usize];
    let (Some(body), Some(owner), false) = (
        function.body,
        function.dispatch_receiver,
        function.is_static,
    ) else {
        return false;
    };
    let Some(class) = ir.class_id_by_name(owner) else {
        return false;
    };
    let fields = crossinline_capture_fields(&ir.classes[class as usize]);
    !fields.is_empty() && reads_own_fields(ir, body, class, &fields)
}

/// The fields of `class` that store a `crossinline` parameter of the inline function it is written
/// in (kotlinc's `FIELD_FOR_CROSSINLINE_CAPTURED_VALUE`), as lowering recorded on each capture.
fn crossinline_capture_fields(class: &IrClass) -> HashSet<u32> {
    class
        .ctor_args
        .iter()
        .filter(|argument| {
            argument
                .capture
                .as_ref()
                .is_some_and(|capture| capture.crossinline_parameter.is_some())
        })
        .filter_map(|argument| argument.field_index)
        .collect()
}

/// Whether `body`, a method of `class`, reads one of `fields` of its own receiver.
fn reads_own_fields(ir: &IrFile, body: ExprId, class: u32, fields: &HashSet<u32>) -> bool {
    let mut pending = vec![body];
    while let Some(expression) = pending.pop() {
        if own_field_read(ir, expression, class, fields) {
            return true;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

/// Whether `expression` reads one of `fields` of the method's own receiver, value 0.
fn own_field_read(ir: &IrFile, expression: ExprId, class: u32, fields: &HashSet<u32>) -> bool {
    matches!(
        ir.expr(expression),
        IrExpr::GetField { receiver, class: owner, index }
            if *owner == class
                && fields.contains(index)
                && matches!(ir.expr(*receiver), IrExpr::GetValue(0))
    )
}

fn is_suspend_inline(ir: &IrFile, fid: u32) -> bool {
    ir.inline_fns.contains(&fid) && ir.suspend_funs.contains(&fid)
}

/// The suspension points of `machine` a body kept for inlining leaves unmarked: the invoke the
/// inlining caller replaces with the lambda's body, of an inline function-typed parameter or of a
/// method's `crossinline` capture.
pub(crate) fn inlined_invokes(ir: &IrFile, fid: u32, machine: &TransformedMachine) -> Vec<ExprId> {
    // A member's value 0 is its receiver; the modes cover the parameters after it. They cover the
    // declared parameters only, not the trailing `$completion` the CPS signature added.
    let function = &ir.functions[fid as usize];
    let receiver = usize::from(!function.is_static);
    let modes = ir
        .fn_params
        .get(&fid)
        .map_or(&[][..], |info| info.expansion_modes.as_slice());
    let captures = function
        .dispatch_receiver
        .filter(|_| for_inline_copy(ir, fid) == Some(ForInlineCopy::CrossinlineCapture))
        .and_then(|owner| ir.class_id_by_name(owner))
        .map(|class| {
            (
                class,
                crossinline_capture_fields(&ir.classes[class as usize]),
            )
        });
    let spliced = |call: ExprId| {
        let IrExpr::InvokeFunction { func, .. } = ir.expr(call) else {
            return false;
        };
        if let Some((class, fields)) = &captures {
            if own_field_read(ir, *func, *class, fields) {
                return true;
            }
        }
        let IrExpr::GetValue(value) = ir.expr(*func) else {
            return false;
        };
        (*value as usize)
            .checked_sub(receiver)
            .and_then(|parameter| modes.get(parameter))
            == Some(&InlineExpansionMode::Splice)
    };
    machine
        .suspensions
        .iter()
        .map(|suspension| suspension.call)
        .filter(|&call| spliced(call))
        .collect()
}
