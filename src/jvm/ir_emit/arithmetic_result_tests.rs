//! The JVM emitter types an arithmetic operator by the result its producer recorded, never by
//! its operands.

use crate::ir::{IrBinOp, IrExpr, IrFile, IrFunction, IrTypeOp};
use crate::jvm::ir_emit::invariant_tests::emit_for_test;
use crate::jvm::ir_emit::EmitRun;
use crate::types::Ty;

/// `fun box(a: Int, b: Long): Long = a + b`, with `a` widened to the selected `Long.plus` operand
/// as checked lowering coerces it, and the sum's result recorded when `recorded`.
fn mixed_sum(recorded: bool) -> IrFile {
    let mut ir = IrFile::default();
    let int = ir.add_expr(IrExpr::GetValue(0));
    let widened = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: int,
        type_operand: Ty::Long,
    });
    let long = ir.add_expr(IrExpr::GetValue(1));
    let sum = if recorded {
        ir.add_arithmetic(IrBinOp::Add, widened, long, Ty::Long)
    } else {
        ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: widened,
            rhs: long,
        })
    };
    let returned = ir.add_expr(IrExpr::Return(Some(sum)));
    ir.add_fun(IrFunction {
        name: "box".into(),
        params: vec![Ty::Int, Ty::Long],
        ret: Ty::Long,
        body: Some(returned),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None, None],
    });
    ir
}

#[test]
fn an_arithmetic_operator_is_emitted_at_its_recorded_result() {
    let run = EmitRun::default();
    let classes = emit_for_test(&mixed_sum(true), "TestKt", &run).expect("emitted class");
    assert_eq!(run.emit_error(), None);
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "TestKt")
        .expect("facade class");
    let code = crate::jvm::classreader::read_method_code(bytes, "box", "(IJ)J").expect("box code");
    // iload_0; i2l; lload_1; ladd; lreturn
    assert_eq!(code.code, [0x1a, 0x85, 0x1f, 0x61, 0xad]);
    assert_eq!(code.max_stack, 4);
}

#[test]
fn an_arithmetic_operator_without_a_recorded_result_is_rejected() {
    let run = EmitRun::default();
    assert!(
        emit_for_test(&mixed_sum(false), "TestKt", &run).is_none(),
        "emission must not derive an unrecorded result from the operands"
    );
    assert_eq!(
        run.emit_error().as_deref(),
        Some("an arithmetic operator has no recorded result type"),
    );
}
