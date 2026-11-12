use super::*;
use std::collections::HashMap;

/// Execute the ordinary field/exchange fragment this test gives the real rewriter. An object is
/// supplied in parameter zero; the interpreter's single field starts at one.
fn evaluate(ir: &IrFile, id: u32, slots: &mut HashMap<u32, i32>, field: &mut i32) -> i32 {
    match ir.expr(id) {
        IrExpr::Const(IrConst::Int(value)) => *value,
        IrExpr::GetValue(slot) => slots[slot],
        IrExpr::Variable {
            index,
            init: Some(value),
            ..
        } => {
            let value = evaluate(ir, *value, slots, field);
            slots.insert(*index, value);
            0
        }
        IrExpr::Block { stmts, value } => {
            for &statement in stmts {
                evaluate(ir, statement, slots, field);
            }
            value.map_or(0, |value| evaluate(ir, value, slots, field))
        }
        IrExpr::GetField { receiver, .. } => {
            assert_eq!(evaluate(ir, *receiver, slots, field), 0);
            *field
        }
        IrExpr::SetField {
            receiver, value, ..
        } => {
            assert_eq!(evaluate(ir, *receiver, slots, field), 0);
            *field = evaluate(ir, *value, slots, field);
            0
        }
        IrExpr::PluginPlaceholder {
            plugin: "atomicfu",
            kind: "exchange",
            exprs,
            ..
        } => {
            assert_eq!(evaluate(ir, exprs[0], slots, field), 0);
            let update = evaluate(ir, exprs[2], slots, field);
            std::mem::replace(field, update)
        }
        other => panic!("unexpected instruction in the exchange fragment: {other:?}"),
    }
}

#[test]
fn capturing_an_atomic_receiver_does_not_overwrite_an_existing_argument_binding() {
    for representation in [
        FieldRepresentation::WasmValue,
        FieldRepresentation::NativeAtomic,
    ] {
        let mut ir = IrFile::default();
        let mut class = crate::ir::test_support::blank_class("Cell");
        class.fields.push(
            crate::ir::IrField::new("value".into(), Ty::Int)
                .with_is_atomic(representation == FieldRepresentation::NativeAtomic),
        );
        ir.classes.push(class);
        let initial = ir.add_expr(IrExpr::Const(IrConst::Int(17)));
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 100_000,
            ty: Ty::Int,
            init: Some(initial),
            named: true,
        });
        let instance = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(instance, Ty::obj("Cell"));
        let argument = ir.add_expr(IrExpr::GetValue(100_000));
        let operation = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let returned = ir.add_expr(IrExpr::GetValue(100_000));
        let root = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, operation],
            value: Some(returned),
        });
        ir.add_fun(crate::ir::IrFunction {
            name: "exchangeThenReadArgument".into(),
            params: vec![Ty::obj("Cell")],
            ret: Ty::Int,
            body: Some(root),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        rewrite(
            &mut ir,
            &[],
            &[DirectOperation {
                expression: operation,
                instance,
                arguments: vec![argument],
                class: 0,
                field: 0,
                value: AtomicValue::Int,
                op: AtomicOp::GetAndSet,
            }],
            representation,
        );
        let types = ir.declared_value_slot_types(root);
        assert_eq!(types[&100_000], Ty::Int);
        let mut slots = HashMap::default();
        slots.insert(0, 0);
        let mut field = 1;
        assert_eq!(evaluate(&ir, root, &mut slots, &mut field), 17);
        assert_eq!(field, 17);
        assert_eq!(slots[&100_000], 17);
    }
}
