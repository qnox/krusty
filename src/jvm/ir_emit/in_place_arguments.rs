//! Emission-side admission for reading an `@InlineOnly` call's arguments in place.

use crate::ir::{Callee, IrBinOp, IrExpr, IrFile, IrIntrinsic, IrTypeOp};
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
        if self.ir.compiled_inline_call(call).is_some()
            || self.machine_suspensions.contains_key(&call)
            || self.ir.suspend_calls.contains_key(&call)
        {
            return false;
        }
        match self.ir.expr(call) {
            // A member of a class in this file: its receiver, arguments and one invoke. An omitted
            // argument goes through the `$default` stub's operands and keeps the stored path.
            IrExpr::MethodCall {
                class, index, args, ..
            } => {
                let function = self.ir.classes[*class as usize].methods[*index as usize];
                args.iter().all(Option::is_some) && !self.ir.suspend_funs.contains(&function)
            }
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(function) | Callee::ClassStatic { function, .. } => {
                    !self.ir.suspend_funs.contains(function)
                }
                Callee::Static { inline, .. } => *inline == InlineKind::None,
                // A sibling file's function is one `invokestatic`, and a member of a sibling-file
                // or classpath class one `invokevirtual`/`invokeinterface` (or the `invokestatic`
                // of a `@JvmStatic` or access bridge) over its sequenced operands.
                Callee::CrossFile { .. } | Callee::Virtual { .. } => true,
                Callee::Intrinsic { operation, .. } => straight_line_intrinsic(operation),
                _ => false,
            },
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
        IrExpr::MethodCall { receiver, args, .. } if shape.plain_invoke(expression) => {
            evaluates_without_local_writes(ir, *receiver, shape)
                && args.iter().all(|argument| {
                    argument
                        .is_some_and(|argument| evaluates_without_local_writes(ir, argument, shape))
                })
        }
        // A statement whose value is discarded before the block's value (`getC().x` of a `const`
        // is `invokestatic getC; pop; bipush 42`) stores nothing while the block declares nothing.
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => {
            arguments_movable(ir, stmts, shape) && evaluates_without_local_writes(ir, *value, shape)
        }
        // A primitive operator is its operands and one instruction, or for a comparison a jump
        // within the argument. kotlinc refuses only a jump that leaves the argument, so
        // `sb.appendLine(a < b)` and `sb.appendLine(if (c) x else y)` stay in place. An eager
        // `And`/`Or` in value position holds its left operand in a `BooleanOperand` temporary
        // (`binary_operation.rs`), so it keeps the stored path; source `&&`/`||` is a `When`.
        IrExpr::PrimitiveBinOp { op, lhs, rhs } if !matches!(op, IrBinOp::And | IrBinOp::Or) => {
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

#[cfg(test)]
mod tests {
    use super::{arguments_movable, EmittedShape};
    use crate::ir::{IrBinOp, IrExpr, IrFile};

    /// Every construction and invoke is plain: only the operator's own emission is in question.
    struct Plain;

    impl EmittedShape for Plain {
        fn plain_construction(&self, _: u32) -> bool {
            true
        }
        fn plain_invoke(&self, _: u32) -> bool {
            true
        }
    }

    fn operator(op: IrBinOp) -> (IrFile, u32) {
        let mut ir = IrFile::default();
        let lhs = ir.add_expr(IrExpr::GetValue(0));
        let rhs = ir.add_expr(IrExpr::GetValue(1));
        let expression = ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs });
        (ir, expression)
    }

    /// Eager `And`/`Or` (range membership, a suspend dispatch) stores its left operand in value
    /// position, so the call keeps its stored arguments. The bitwise `and` stores nothing.
    #[test]
    fn an_eager_boolean_connective_keeps_the_stored_path() {
        for op in [IrBinOp::And, IrBinOp::Or] {
            let (ir, expression) = operator(op);
            assert!(!arguments_movable(&ir, &[expression], &Plain), "{op:?}");
        }
        let (ir, expression) = operator(IrBinOp::BitAnd);
        assert!(arguments_movable(&ir, &[expression], &Plain));
    }
}
