//! The JVM's type for a temporary the shared suspend normalizations bind an operand to.
//!
//! Hoisting a suspension out of an operand list evaluates the earlier operands into temporaries
//! first, and the JVM's temporaries carry the verifier type kotlinc gives them. This pass runs after
//! JVM lowering has made calls physical, so a callee's result is read from its descriptor where no
//! source declaration remains.

use std::collections::HashMap;

use crate::ir::{Callee, ExprId, IrBinOp, IrConst, IrExpr, IrFile};
use crate::types::Ty;

/// The JVM value type of a temporary that holds an already-evaluated operand ahead of a later
/// suspension. This is deliberately an IR-identity query: it reads the selected
/// callee/field/type node and never re-resolves a source name. It covers the common runtime-read and
/// expression shapes that can precede a suspension in any ordered operand list; returning `None` makes
/// an unexpected lowering shape decline safely instead of emitting a temp with a guessed verifier type.
pub(super) fn snapshot_type(
    ir: &IrFile,
    expression: ExprId,
    orig_rets: &[Ty],
    value_types: &HashMap<u32, Ty>,
) -> Option<Ty> {
    match &ir.exprs[expression as usize] {
        IrExpr::Const(constant) => Some(match constant {
            IrConst::Boolean(_) => Ty::Boolean,
            IrConst::UByte(_) => Ty::UByte,
            IrConst::UShort(_) => Ty::UShort,
            IrConst::UInt(_) => Ty::UInt,
            IrConst::ULong(_) => Ty::ULong,
            IrConst::Int(_) => Ty::Int,
            IrConst::Long(_) => Ty::Long,
            IrConst::Double(_) => Ty::Double,
            IrConst::Float(_) => Ty::Float,
            IrConst::Char(_) => Ty::Char,
            IrConst::String(_) => Ty::String,
            IrConst::Short(_) => Ty::Short,
            IrConst::Byte(_) => Ty::Byte,
            IrConst::Null => Ty::Null,
        }),
        // A class literal is emitted as an `ldc Class`, which is still a runtime resolution action and
        // therefore must stay before a later suspension (including any linkage failure it can raise).
        IrExpr::ClassConst { .. } => Some(Ty::obj("java/lang/Class")),
        // Value indices are local to one function. Looking through the complete arena can find a
        // declaration with the same numeric index in an unrelated method and poison the verifier type
        // of the new temp. The caller supplies the current function's parameter/local environment.
        IrExpr::GetValue(index) => value_types.get(index).copied(),
        IrExpr::Call { callee, .. } => {
            if let Some(function) = callee.source_function() {
                orig_rets.get(function as usize).copied()
            } else {
                match callee {
                    Callee::CrossFile { ret, .. }
                    | Callee::Module { ret, .. }
                    | Callee::Super { ret, .. }
                    | Callee::External { ret, .. } => Some(*ret),
                    Callee::Static { .. } | Callee::Special { .. } => physical_call_result(callee),
                    Callee::Virtual { params, .. } => params
                        .as_ref()
                        .map(|(_, ret)| *ret)
                        .or_else(|| physical_call_result(callee)),
                    Callee::Intrinsic { ret, .. } => Some(*ret),
                    _ => unreachable!("source-function callees were handled above"),
                }
            }
        }
        IrExpr::MethodCall { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.methods.get(*index as usize))
            .and_then(|function| orig_rets.get(*function as usize))
            .copied(),
        IrExpr::InvokeFunction { ret, .. } => Some(*ret),
        IrExpr::Equality { .. } => Some(Ty::Boolean),
        IrExpr::PrimitiveBinOp { op, lhs, .. } => Some(match op {
            IrBinOp::Lt
            | IrBinOp::Le
            | IrBinOp::Gt
            | IrBinOp::Ge
            | IrBinOp::Eq
            | IrBinOp::Ne
            | IrBinOp::RefEq
            | IrBinOp::RefNe
            | IrBinOp::And
            | IrBinOp::Or => Ty::Boolean,
            _ => snapshot_type(ir, *lhs, orig_rets, value_types)?,
        }),
        IrExpr::PrimitiveNeg { ty, .. }
        | IrExpr::TypeOp {
            type_operand: ty, ..
        } => Some(*ty),
        // A constructed instance's verifier type is its class; generic arguments are erased at this
        // boundary (the temp only needs the internal name).
        IrExpr::New { internal, .. } => Some(Ty::Obj(*internal, &[])),
        // `operand!!` yields its operand's value unchanged (the assert only throws), matching
        // `value_ty`'s treatment in the emitter.
        IrExpr::BottomValue { .. } => Some(Ty::Nothing),
        IrExpr::NotNullAssert { operand, .. } => {
            snapshot_type(ir, *operand, orig_rets, value_types)
        }
        // A value block is an evaluation wrapper, not a distinct value representation. Hoisting its
        // statements collapses the wrapper to the final expression, so a snapshot uses that final
        // expression's exact type. An empty/statement-only block has no value to materialize.
        IrExpr::Block {
            value: Some(value), ..
        } => snapshot_type(ir, *value, orig_rets, value_types),
        // Declared storage types, read straight off the IR declaration the node indexes — the same
        // sources the emitter's `value_ty` consults. `PropertyRead` carries its type inline; a
        // `Unit`-typed property realizes through a `()V` accessor (nothing to bind) and a bare
        // type-parameter's erasure is the emitter's concern, so both decline.
        IrExpr::GetStatic(i) => Some(ir.statics[*i as usize].ty),
        // Static/singleton reads share the same semantic rule regardless of whether their declaration
        // originated in this file, another module, or the classpath. Recover their physical type from
        // the identity already stored on the IR node; the descriptor parser is shared with emission so
        // array/object/primitive handling cannot drift into a suspend-only copy.
        IrExpr::StaticInstance { ty, .. } => ir
            .classes
            .get(*ty as usize)
            .map(|class| Ty::obj_name(class.fq_name)),
        IrExpr::ExternalStaticInstance { ty, .. } => Some(Ty::obj_name(*ty)),
        IrExpr::ExternalStaticField { descriptor, .. } => static_field_type(descriptor),
        IrExpr::EnumEntry { classifier, .. } => Some(Ty::obj_name(*classifier)),
        IrExpr::EnumValueOf { classifier, .. } => Some(Ty::obj_name(*classifier)),
        IrExpr::EnumValues { classifier } => Some(Ty::array(Ty::obj_name(*classifier))),
        IrExpr::EnumEntries { classifier } => Some(Ty::obj_args_name(
            crate::types::type_name("kotlin/enums/EnumEntries"),
            &[Ty::obj_name(*classifier)],
        )),
        IrExpr::UnitInstance => Some(Ty::obj("kotlin/Unit")),
        IrExpr::GetField { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.fields.get(*index as usize))
            .map(|field| field.ty),
        IrExpr::EnclosingInstance { outer, .. } => Some(Ty::obj_name(*outer)),
        IrExpr::LateinitInitialized { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.fields.get(*index as usize))
            .map(|field| field.ty),
        IrExpr::PropertyRead { ty, .. } => {
            (!matches!(ty, Ty::Unit | Ty::Error | Ty::TyParam(..))).then_some(*ty)
        }
        IrExpr::RefGet { elem, .. } => Some(*elem),
        _ => None,
    }
}

/// The result of a call JVM lowering already made physical, from its method descriptor.
fn physical_call_result(callee: &Callee) -> Option<Ty> {
    match callee {
        Callee::Static { descriptor, .. }
        | Callee::Special { descriptor, .. }
        | Callee::Virtual { descriptor, .. } => {
            crate::jvm::ir_emit::parse_physical_method_desc(descriptor).map(|(_, ret)| ret)
        }
        _ => None,
    }
}

/// The value type of a dependency static field, from its field descriptor.
fn static_field_type(descriptor: &str) -> Option<Ty> {
    let ty = crate::jvm::ir_emit::ty_from_field_descriptor(descriptor);
    (!matches!(ty, Ty::Unit | Ty::Error)).then_some(ty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn a_static_instance_uses_the_stored_class_identity() {
        let mut ir = IrFile::default();
        ir.classes
            .push(crate::ir::test_support::blank_class("example/Outer$Widget"));
        let instance = ir.add_expr(IrExpr::StaticInstance {
            owner: 0,
            ty: 0,
            field: "INSTANCE",
        });
        let ty = snapshot_type(&ir, instance, &[], &HashMap::new()).expect("static instance type");
        assert_eq!(ty, Ty::obj_name(type_name("example/Outer$Widget")));
    }
}
