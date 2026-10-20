//! Narrowing the erased `Object` a suspension resumes with to the result's declared type.

use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::{Ty, TyKind};

/// Coerce an `Object` value to `target` (unbox a primitive, or checkcast a reference).
pub(super) fn unbox(ir: &mut IrFile, value: ExprId, target: &Ty) -> ExprId {
    // The CPS resume value is `Object`; a reference target (`Config`, `String`, `List<…>`) needs a real
    // `checkcast` to that type, while a primitive target unboxes. `ImplicitCoercion` unboxes but does not
    // narrow a reference, so a concrete reference result would otherwise stay `Object` (VerifyError at its
    // first typed use). `Cast` (a plain `checkcast`, null-passing) applies for a reference; `kotlin/Any`
    // needs neither (already `Object`).
    let op = if reference_needs_checkcast(*target) {
        IrTypeOp::Cast
    } else {
        IrTypeOp::ImplicitCoercion
    };
    ir.add_expr(IrExpr::TypeOp {
        op,
        arg: value,
        type_operand: target.clone(),
    })
}

/// Whether narrowing an erased `Object` resume value to `t` needs an explicit `checkcast` — a concrete
/// reference class (`Config`), `String`, or an array. `kotlin/Any` (already `Object`) and primitives do
/// NOT; crucially a BOXED-primitive object type (`Obj("kotlin/Int")`, a spilled `Int`) also does not —
/// there `ImplicitCoercion` UNBOXES to the primitive, whereas a `checkcast` would leave it boxed and a
/// later primitive use (`istore`/`iadd`) would fail verification.
fn reference_needs_checkcast(t: Ty) -> bool {
    match t.kind() {
        // A NULLABLE PRIMITIVE (`Int?`) is a boxed reference (`Integer`): the resume value must be
        // checkcast to the wrapper — `ImplicitCoercion` would no-op (it can't unbox to a nullable),
        // leaving the slot `Object` while the spill restore's frame type is the wrapper (VerifyError
        // at the state merge). A nullable REFERENCE keeps the inner type's own answer.
        TyKind::Nullable(inner) => {
            t.nullable_primitive().is_some() || reference_needs_checkcast(inner)
        }
        TyKind::TyParam(_, inner) => reference_needs_checkcast(inner),
        TyKind::STRING => true,
        TyKind::Obj(i, _) => {
            i != crate::types::wk::any()
                && !crate::jvm::jvm_class_map::is_boxed_primitive_classifier(i)
        }
        _ => false,
    }
}
