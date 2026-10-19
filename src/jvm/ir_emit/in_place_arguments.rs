//! Emission-side admission for reading an `@InlineOnly` call's arguments in place.

use crate::ir::{Callee, IrExpr, IrFile, IrIntrinsic, IrTypeOp};
use crate::libraries::InlineKind;

use super::Emitter;

/// The facts about an argument's code that only emission knows: whether a node is written as
/// straight-line code with no local of its own.
pub(super) trait EmittedShape {
    /// A `New` is written as one constructor invoke, not as a guarded construction such as a
    /// nullable SAM wrapper's null check.
    fn plain_construction(&self, construction: u32) -> bool;
    /// A call is written as its operands and one invoke or instruction: it is not spliced from an
    /// inline body and is not a suspension point.
    fn plain_invoke(&self, call: u32) -> bool;
}

impl EmittedShape for Emitter<'_> {
    fn plain_construction(&self, construction: u32) -> bool {
        !self
            .sam_wrapper_realizations
            .is_nullable_construction(construction)
    }

    fn plain_invoke(&self, call: u32) -> bool {
        let IrExpr::Call { callee, .. } = self.ir.expr(call) else {
            return false;
        };
        if self.ir.compiled_inline_call(call).is_some()
            || self.machine_suspensions.contains_key(&call)
            || self.ir.suspend_calls.contains_key(&call)
        {
            return false;
        }
        match callee {
            Callee::Local(function) | Callee::ClassStatic { function, .. } => {
                !self.ir.suspend_funs.contains(function)
            }
            Callee::Static { inline, .. } => *inline == InlineKind::None,
            Callee::Intrinsic { operation, .. } => straight_line_intrinsic(operation),
            _ => false,
        }
    }
}

/// An intrinsic the backend writes as its operands and an instruction or invoke, with no
/// temporary: an array or string access, a comparison's `compare`, an enum's `name()`, a
/// `toString` of a nullable or unsigned value, a primitive array's `newarray`. An intrinsic that
/// splices an inline template (`enumValueOf`, `enumEntries`), throws (`assert`), builds a type
/// (`typeOf`) or tests nullable floating-point equality keeps the stored path.
fn straight_line_intrinsic(operation: &IrIntrinsic) -> bool {
    matches!(
        operation,
        IrIntrinsic::ArrayGet
            | IrIntrinsic::ArraySet
            | IrIntrinsic::ArraySize
            | IrIntrinsic::StringGet
            | IrIntrinsic::StringLength
            | IrIntrinsic::EnumName
            | IrIntrinsic::NullableAnyToString
            | IrIntrinsic::PrimitiveCompare { .. }
            | IrIntrinsic::UnsignedCompare { .. }
            | IrIntrinsic::UnsignedToString { .. }
            | IrIntrinsic::PrimitiveArrayNew { .. }
    )
}

/// Whether every argument's code can be moved to where the body loads it: kotlinc refuses the
/// move for a whole call when any argument's code stores a local or jumps out of itself.
pub(super) fn arguments_movable(ir: &IrFile, arguments: &[u32], shape: &dyn EmittedShape) -> bool {
    arguments
        .iter()
        .all(|&argument| evaluates_without_local_writes(ir, argument, shape))
}

/// Whether an expression's emitted code can neither store a local nor jump outside itself.
/// Deliberately narrow: an unlisted shape retains the ordinary stored-parameter path.
fn evaluates_without_local_writes(ir: &IrFile, expression: u32, shape: &dyn EmittedShape) -> bool {
    match ir.expr(expression) {
        IrExpr::GetValue(_)
        | IrExpr::Const(_)
        | IrExpr::GetStatic(_)
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::EnclosingInstance { .. }
        | IrExpr::UnitInstance => true,
        // `new; dup; <arguments>; invokespecial`: a direct construction stores nothing either.
        IrExpr::New { args, defaults, .. }
            if defaults.is_empty() && shape.plain_construction(expression) =>
        {
            arguments_movable(ir, args, shape)
        }
        // An ordinary function or getter invoke stores nothing: kotlinc's `sb.appendLine(word())`
        // keeps `sb` on the stack while `word()` runs where `appendLine`'s body loads its argument.
        IrExpr::Call {
            dispatch_receiver,
            args,
            ..
        } if shape.plain_invoke(expression) => {
            dispatch_receiver
                .is_none_or(|receiver| evaluates_without_local_writes(ir, receiver, shape))
                && arguments_movable(ir, args, shape)
        }
        // A statement whose value is discarded before the block's value (`getC().x` of a `const`
        // is `invokestatic getC; pop; bipush 42`) stores nothing while the block declares nothing.
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => {
            arguments_movable(ir, stmts, shape) && evaluates_without_local_writes(ir, *value, shape)
        }
        // A primitive operator is its operands and one instruction, or for a comparison or logical
        // connective a jump within the argument. kotlinc refuses only a jump that leaves the
        // argument, so `sb.appendLine(a < b)` and `sb.appendLine(if (c) x else y)` stay in place.
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => {
            evaluates_without_local_writes(ir, *lhs, shape)
                && evaluates_without_local_writes(ir, *rhs, shape)
        }
        IrExpr::PrimitiveNeg { operand, .. } => evaluates_without_local_writes(ir, *operand, shape),
        IrExpr::When { branches } => branches.iter().all(|&(condition, result)| {
            condition.is_none_or(|condition| evaluates_without_local_writes(ir, condition, shape))
                && evaluates_without_local_writes(ir, result, shape)
        }),
        // `getfield`, a property accessor's invoke and a `!!` check store nothing and stay in place:
        // kotlinc's `h.items += F()` evaluates the receiver where `plusAssign`'s body loads it. A
        // `lateinit` field read's guard branches only within the argument, which kotlinc moves too.
        IrExpr::GetField { receiver, .. }
        | IrExpr::NotNullAssert {
            operand: receiver, ..
        } => evaluates_without_local_writes(ir, *receiver, shape),
        IrExpr::PropertyRead { receiver, .. } => {
            receiver.is_none_or(|receiver| evaluates_without_local_writes(ir, receiver, shape))
        }
        IrExpr::TypeOp {
            op: IrTypeOp::Cast | IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => evaluates_without_local_writes(ir, *arg, shape),
        // An empty vararg is `iconst_0; anewarray` (or `newarray`). Nothing is stored, so an
        // `@InlineOnly` body that loads the parameter twice can duplicate the array on the stack.
        // An element makes the builder store the array, and that call keeps its parameter slots.
        IrExpr::Vararg { elements, .. } if elements.is_empty() => true,
        _ => false,
    }
}
