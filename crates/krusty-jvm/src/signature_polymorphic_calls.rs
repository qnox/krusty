//! The JVM realization of a call to a signature-polymorphic method (`MethodHandle.invoke`,
//! `VarHandle.get`, …).
//!
//! The JVM resolves such a method against the descriptor of the call instruction, not against the
//! declaration's `(Object...)Object`. kotlinc therefore passes the arguments as they are, spread
//! out and not packed into an array, and spells the descriptor from the static types of those
//! arguments. A declaration whose result is `Object` takes the result type of the `as` cast the
//! call is the operand of, so `handle.get(target) as Int` is `(…)I`; a declaration with any other
//! result keeps it.
//!
//! The selected declaration states the shape: its provider records it signature-polymorphic
//! (`LibraryMember::is_signature_polymorphic`). Common IR keeps the ordinary call with its single
//! `vararg` argument; this module owns the physical form.

use krusty::ir::{ExprId, IrExpr, IrFile, IrTypeOp};
use krusty::types::Ty;

/// The `as` cast reading each call, with its target, by that call: a signature-polymorphic call
/// takes its `Object` result type from the cast that reads it, and the cast is then redundant.
pub(super) fn cast_targets(ir: &IrFile) -> krusty::fx_hash::HashMap<ExprId, (ExprId, Ty)> {
    let mut targets = krusty::fx_hash::HashMap::default();
    for (index, expression) in ir.exprs.iter().enumerate() {
        if let IrExpr::TypeOp {
            op: IrTypeOp::Cast | IrTypeOp::CastNonNull,
            arg,
            type_operand,
        } = expression
        {
            if matches!(ir.expr(*arg), IrExpr::Call { .. }) {
                targets.insert(*arg, (index as ExprId, *type_operand));
            }
        }
    }
    targets
}

/// A signature-polymorphic call in its physical form.
pub(super) struct PolymorphicCall {
    /// The call instruction's descriptor.
    pub(super) descriptor: String,
    /// The operands, in order, one per descriptor parameter.
    pub(super) arguments: Vec<ExprId>,
    /// The checked type of each operand.
    pub(super) parameters: Vec<Ty>,
    /// The result the instruction leaves, which is also the type of the call.
    pub(super) result: Ty,
}

/// Realize the call whose single `vararg` operand is `arguments`. `declared_result` is the
/// declaration's physical result and `cast` the type of the `as` cast reading the call, if any.
pub(super) fn realize(
    ir: &IrFile,
    arguments: &[ExprId],
    declared_result: Ty,
    cast: Option<Ty>,
) -> Result<PolymorphicCall, String> {
    let [packed] = arguments else {
        return Err(format!(
            "has {} operands where a signature-polymorphic call packs one `vararg`",
            arguments.len()
        ));
    };
    let IrExpr::Vararg {
        spreads, elements, ..
    } = ir.expr(*packed)
    else {
        return Err("does not pass its operands as a `vararg`".to_string());
    };
    if spreads.iter().any(|spread| *spread) {
        return Err("spreads an array into a signature-polymorphic call".to_string());
    }
    let mut operands = Vec::with_capacity(elements.len());
    let mut parameters = Vec::with_capacity(elements.len());
    for element in elements {
        let operand = unpacked(ir, *element);
        let ty = ir
            .checked_type(operand)
            .ok_or_else(|| format!("operand {operand} has no checked type"))?;
        operands.push(operand);
        // `null` has no class of its own: the descriptor spells it as `Void`, as kotlinc does.
        // `Unit` is a value here, the `kotlin.Unit` singleton: `void` is only a result.
        parameters.push(match ty {
            Ty::Null => Ty::Nothing,
            Ty::Unit => krusty::jvm::method_descriptors::jvm_value_ty(&ty),
            ty => ty,
        });
    }
    let declares_object = krusty::jvm::names::type_descriptor(declared_result)
        == krusty::jvm::names::type_descriptor(Ty::obj_name(krusty::types::wk::java_object()));
    let result = match cast {
        Some(target) if declares_object && !matches!(target, Ty::Unit | Ty::Nothing) => target,
        _ => declared_result,
    };
    Ok(PolymorphicCall {
        descriptor: krusty::jvm::names::method_descriptor(&parameters, result),
        arguments: operands,
        parameters,
        result,
    })
}

/// The operand as written: packing it into `Object...` coerced it to the element type, which
/// is not the type the descriptor spells.
fn unpacked(ir: &IrFile, mut operand: ExprId) -> ExprId {
    while let IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg,
        type_operand,
    } = ir.expr(operand)
    {
        if krusty::jvm::names::type_descriptor(*type_operand)
            != krusty::jvm::names::type_descriptor(Ty::obj_name(krusty::types::wk::java_object()))
        {
            break;
        }
        operand = *arg;
    }
    operand
}
