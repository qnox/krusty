//! Box and carrier roles of value-class type operations.
//!
//! The value-class adaptation boundary records the exact unbox call -> type operation edge it
//! creates. Once the IR is final, this module consumes that provenance and records which classfile
//! name each primitive-array value-class type operation uses. Emission reads the record.

use super::representation::{carrier_slot, primitive_array_value_class};
use super::Under;
use crate::ir::{
    Callee, ExprId, IrExpr, IrFile, IrTypeOp, IrValueClassTypeOperation, IrValueClassTypeRole,
};
use crate::types::Ty;
use std::collections::HashSet;

/// Record a generated value-class unbox call whose receiver is a type operation. Other receiver
/// shapes need no preservation during type erasure and therefore create no type-operation edge.
pub(super) fn record_unbox_type_operation_edge(ir: &mut IrFile, call: ExprId, receiver: ExprId) {
    if !matches!(ir.exprs.get(receiver as usize), Some(IrExpr::TypeOp { .. })) {
        return;
    }
    assert!(
        matches!(
            ir.exprs.get(call as usize),
            Some(IrExpr::Call {
                dispatch_receiver: Some(found),
                ..
            }) if *found == receiver
        ),
        "a value-class unbox edge must connect its call to its receiver"
    );
    assert!(
        ir.value_class_unbox_type_operation_edges
            .insert(call, receiver)
            .is_none_or(|recorded| recorded == receiver),
        "a value-class unbox call has one exact type-operation receiver"
    );
}

/// `call` is about to be replaced, so it is no longer the unbox operation this edge named.
pub(super) fn retire_unbox_type_operation_edge(ir: &mut IrFile, call: ExprId) {
    ir.value_class_unbox_type_operation_edges.remove(&call);
}

/// Exact type-operation receivers of generated value-class unbox calls still present in the IR.
pub(super) fn recorded_unbox_type_operation_receivers(
    ir: &IrFile,
) -> impl Iterator<Item = ExprId> + '_ {
    ir.value_class_unbox_type_operation_edges
        .iter()
        .map(|(&call, &receiver)| {
            assert!(
                matches!(
                    ir.exprs.get(call as usize),
                    Some(IrExpr::Call {
                        dispatch_receiver: Some(found),
                        ..
                    }) if *found == receiver
                ),
                "a recorded value-class unbox edge must still connect its call to its receiver"
            );
            assert!(
                matches!(ir.exprs.get(receiver as usize), Some(IrExpr::TypeOp { .. })),
                "a recorded value-class unbox receiver must still be a type operation"
            );
            receiver
        })
}

/// A primitive-array allocation has no method descriptor. Record its carrier so a later result
/// classification can tell that fact from a missing descriptor.
pub(super) fn record_primitive_array_allocation_carriers(ir: &mut IrFile, under: &Under) {
    let allocations = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, expr)| {
            let IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::PrimitiveArrayNew { element },
                        ..
                    },
                ..
            } = expr
            else {
                return None;
            };
            let carrier = carrier_slot(Ty::array(*element), under)?;
            Some((id as ExprId, carrier))
        })
        .collect::<Vec<_>>();
    for (id, carrier) in allocations {
        ir.physical_types.entry(id).or_insert(carrier);
    }
}

/// Record the classfile role of every type operation on a primitive-array value class.
pub(super) fn record_primitive_array_type_operations(ir: &mut IrFile, under: &Under) {
    let receivers = recorded_unbox_type_operation_receivers(ir).collect::<HashSet<_>>();
    ir.value_class_unbox_type_operation_edges.clear();
    for (id, expr) in ir.exprs.iter().enumerate() {
        let IrExpr::TypeOp {
            op, type_operand, ..
        } = expr
        else {
            continue;
        };
        let Some(name) = type_operand.non_null().obj_internal() else {
            continue;
        };
        let Some((boxed_owner, carrier)) = primitive_array_value_class(name, under) else {
            continue;
        };
        let id = id as ExprId;
        let role = match op {
            IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => IrValueClassTypeRole::Box,
            _ if receivers.contains(&id) => IrValueClassTypeRole::Box,
            _ => IrValueClassTypeRole::Carrier,
        };
        ir.value_class_type_operations.insert(
            id,
            IrValueClassTypeOperation {
                boxed_owner,
                carrier,
                role,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrTypeOp;
    use crate::types::Ty;

    #[test]
    fn a_recorded_unbox_edge_distinguishes_an_ordinary_same_spelling_call() {
        let array = crate::types::type_name("kotlin/UIntArray");
        let carrier = Ty::obj("kotlin/IntArray");
        let mut under = Under::new();
        under.insert(array, carrier);
        let mut ir = IrFile::default();
        let arg = ir.add_expr(IrExpr::GetValue(0));
        super::super::unboxing_rewrites::unbox_wrap(&mut ir, arg, array, &under);
        let unbox_cast = ir.value_class_unbox_type_operation_edges[&arg];
        let same_spelling_arg = ir.add_expr(IrExpr::GetValue(1));
        let same_spelling_cast = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: same_spelling_arg,
            type_operand: Ty::obj_name(array),
        });
        ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: crate::types::type_name("sample/Ordinary"),
                name: "unbox-impl".to_string(),
                descriptor: "()[I".to_string(),
                params: None,
                interface: false,
                module_target: None,
            },
            dispatch_receiver: Some(same_spelling_cast),
            args: vec![],
        });
        let instance = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: same_spelling_arg,
            type_operand: Ty::obj_name(array),
        });

        record_primitive_array_type_operations(&mut ir, &under);

        let name = |id| {
            crate::jvm::value_classes::representation::type_operation_internal_name(
                ir.value_class_type_operations[&id],
            )
        };
        assert_eq!(
            ir.value_class_type_operations[&unbox_cast].role,
            IrValueClassTypeRole::Box
        );
        assert_eq!(name(unbox_cast), "kotlin/UIntArray");
        assert_eq!(
            ir.value_class_type_operations[&same_spelling_cast].role,
            IrValueClassTypeRole::Carrier
        );
        assert_eq!(name(same_spelling_cast), "[I");
        assert_eq!(
            ir.value_class_type_operations[&instance].role,
            IrValueClassTypeRole::Box
        );
        assert_eq!(name(instance), "kotlin/UIntArray");
        assert_eq!(
            crate::jvm::names::type_descriptor(Ty::obj_name(array)),
            crate::jvm::names::type_descriptor(carrier)
        );
    }

    #[test]
    fn replacing_a_recorded_unbox_call_drops_its_type_operation_edge() {
        let array = crate::types::type_name("kotlin/UIntArray");
        let carrier = Ty::obj("kotlin/IntArray");
        let mut under = Under::new();
        under.insert(array, carrier);
        let mut ir = IrFile::default();
        let arg = ir.add_expr(IrExpr::GetValue(0));
        super::super::unboxing_rewrites::unbox_wrap(&mut ir, arg, array, &under);
        let unbox_cast = ir.value_class_unbox_type_operation_edges[&arg];
        ir.exprs[arg as usize] = IrExpr::Const(crate::ir::IrConst::Int(0));
        retire_unbox_type_operation_edge(&mut ir, arg);

        record_primitive_array_type_operations(&mut ir, &under);

        assert_eq!(
            ir.value_class_type_operations[&unbox_cast].role,
            IrValueClassTypeRole::Carrier
        );
    }
}
