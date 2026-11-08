use crate::ir::{Callee, IrClass, IrConst, IrExpr, IrFile, IrFunction, IrIntrinsic};
use crate::types::Ty;
use crate::wasm::WasmTarget;

fn value_function(ir: &mut IrFile, name: &str, ret: Ty, value: u32) -> u32 {
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    ir.set_logical_type(returned, Ty::Nothing);
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    ir.set_logical_type(body, Ty::Unit);
    ir.add_fun(IrFunction {
        name: name.to_owned(),
        params: Vec::new(),
        ret,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    })
}

#[test]
fn an_unrelated_emitted_lambda_does_not_prevent_hashing_an_erased_primitive() {
    for target in WasmTarget::ALL {
        let mut ir = IrFile::default();
        let seven = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
        ir.set_logical_type(seven, Ty::Int);
        let implementation = value_function(&mut ir, "constant", Ty::Int, seven);
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });
        let function_type = Ty::fun(Vec::new(), Ty::Int);
        ir.set_logical_type(lambda, function_type);
        value_function(&mut ir, "unrelated", function_type, lambda);

        let hashed = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::Intrinsic {
                operation: IrIntrinsic::GeneratedPropertyHash {
                    ty: Ty::obj_name(crate::types::wk::any()),
                },
                ret: Ty::Int,
            }),
            dispatch_receiver: None,
            args: vec![seven],
        });
        ir.set_logical_type(hashed, Ty::Int);
        value_function(&mut ir, "hash", Ty::Int, hashed);
        let properties = crate::backend::local_properties::realize(&mut ir).unwrap();
        let mut started = super::super::super::Started::new(target);
        assert_eq!(
            super::super::lower_alone(&ir, &properties, &mut started).map(|_| ()),
            Ok(())
        );
    }
}

#[test]
fn a_unit_property_hash_does_not_dispatch_through_any() {
    for target in WasmTarget::ALL {
        for ty in [Ty::Unit, Ty::obj_name(crate::types::wk::unit())] {
            let mut ir = IrFile::default();
            // A neighbor that still inherits `Any.hashCode` is what used to make every reference
            // property hash look like it might reach that unimplemented slot.
            ir.classes
                .push(IrClass::synthetic(crate::types::type_name("demo/Neighbor")));
            let unit = ir.add_expr(IrExpr::UnitInstance);
            ir.set_logical_type(unit, ty);
            let hashed = ir.add_expr(IrExpr::Call {
                callee: Box::new(Callee::Intrinsic {
                    operation: IrIntrinsic::GeneratedPropertyHash { ty },
                    ret: Ty::Int,
                }),
                dispatch_receiver: None,
                args: vec![unit],
            });
            ir.set_logical_type(hashed, Ty::Int);
            value_function(&mut ir, "hash", Ty::Int, hashed);
            let properties = crate::backend::local_properties::realize(&mut ir).unwrap();
            let mut started = super::super::super::Started::new(target);
            assert_eq!(
                super::super::lower_alone(&ir, &properties, &mut started).map(|_| ()),
                Ok(())
            );
        }
    }
}
