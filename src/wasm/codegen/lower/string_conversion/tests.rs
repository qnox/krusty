use crate::ir::{Callee, IrConst, IrExpr, IrFile, IrFunction, IrIntrinsic, IrTypeOp};
use crate::types::Ty;
use crate::wasm::WasmTarget;

fn lower(mut ir: IrFile, value: u32, target: WasmTarget) -> Result<(), String> {
    ir.add_fun(IrFunction {
        name: "decimal".into(),
        params: Vec::new(),
        ret: Ty::String,
        body: Some(value),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let properties = crate::backend::local_properties::realize(&mut ir).unwrap();
    let mut started = super::super::super::Started::new(target);
    super::super::lower_alone(&ir, &properties, &mut started).map(|_| ())
}

fn receiver(ir: &mut IrFile, ty: Ty) -> u32 {
    let constant = match ty {
        Ty::ULong => IrConst::ULong(u64::MAX),
        _ => IrConst::UInt(u32::MAX),
    };
    let value = ir.add_expr(IrExpr::Const(constant));
    ir.set_logical_type(value, ty);
    value
}

fn conversion(ir: &mut IrFile, source: Ty, receiver: Option<u32>, args: Vec<u32>) -> u32 {
    let value = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Intrinsic {
            operation: IrIntrinsic::UnsignedToString { source },
            ret: Ty::String,
        }),
        dispatch_receiver: receiver,
        args,
    });
    ir.set_logical_type(value, Ty::String);
    value
}

#[test]
fn all_four_unsigned_scalar_widths_lower_through_templates_and_the_selected_intrinsic() {
    for target in WasmTarget::ALL {
        for ty in [Ty::UByte, Ty::UShort, Ty::UInt, Ty::ULong] {
            for intrinsic in [false, true] {
                let mut ir = IrFile::default();
                let receiver = receiver(&mut ir, ty);
                let value = if intrinsic {
                    conversion(&mut ir, ty, Some(receiver), Vec::new())
                } else {
                    ir.add_expr(IrExpr::StringConcat(vec![receiver]))
                };
                ir.set_logical_type(value, Ty::String);
                assert_eq!(lower(ir, value, target), Ok(()), "{ty:?}");
            }
        }
    }
}

#[test]
fn unsigned_decimal_intrinsics_require_the_exact_receiver_type_and_operand_frame() {
    for target in WasmTarget::ALL {
        for (source, actual) in [
            (Ty::UInt, Ty::Int),
            (Ty::UByte, Ty::UInt),
            (Ty::ULong, Ty::UInt),
            (Ty::Int, Ty::Int),
            (Ty::nullable(Ty::UInt), Ty::nullable(Ty::UInt)),
        ] {
            let mut ir = IrFile::default();
            let receiver = receiver(&mut ir, actual);
            let value = conversion(&mut ir, source, Some(receiver), Vec::new());
            assert_eq!(
                lower(ir, value, target),
                Err(
                    "an unsigned string conversion without its exact non-null scalar source".into()
                ),
            );
        }
        for (has_receiver, has_argument) in [(false, false), (false, true), (true, true)] {
            let mut ir = IrFile::default();
            let receiver = receiver(&mut ir, Ty::UInt);
            let args = if has_argument {
                vec![receiver]
            } else {
                Vec::new()
            };
            let value = conversion(&mut ir, Ty::UInt, has_receiver.then_some(receiver), args);
            assert_eq!(
                lower(ir, value, target),
                Err("an unsigned string conversion with malformed operands".into()),
            );
        }
    }
}

#[test]
fn unsigned_decimal_support_does_not_admit_nullable_or_erased_unsigned_boxes() {
    for target in WasmTarget::ALL {
        for ty in [Ty::UByte, Ty::UShort, Ty::UInt, Ty::ULong] {
            let mut ir = IrFile::default();
            let null = ir.add_expr(IrExpr::Const(IrConst::Null));
            ir.set_logical_type(null, Ty::nullable(ty));
            let value = ir.add_expr(IrExpr::StringConcat(vec![null]));
            ir.set_logical_type(value, Ty::String);
            assert_eq!(
                lower(ir, value, target),
                Err(format!("a string template part of type `{ty:?}?`")),
            );

            let mut ir = IrFile::default();
            let scalar = receiver(&mut ir, ty);
            let erased = ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: scalar,
                type_operand: Ty::obj_name(crate::types::wk::any()),
            });
            ir.set_logical_type(erased, Ty::obj_name(crate::types::wk::any()));
            let value = ir.add_expr(IrExpr::StringConcat(vec![erased]));
            ir.set_logical_type(value, Ty::String);
            assert_eq!(
                lower(ir, value, target),
                Err(format!("a `{ty:?}` value carried as a reference")),
            );
        }
    }
}
