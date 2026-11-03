//! The JVM emitter types a value `when` by the result its producer recorded, never by its
//! branches.

use crate::ir::{IrConst, IrExpr, IrFile, IrFunction};
use crate::jvm::ir_emit::invariant_tests::emit_for_test;
use crate::jvm::ir_emit::EmitRun;
use crate::types::Ty;

/// `fun box(c: Boolean): Any = if (c) "a" else 1`, with the `when`'s result recorded as `result`
/// when there is one.
fn conditional_function(result: Option<Ty>) -> IrFile {
    let mut ir = IrFile::default();
    let condition = ir.add_expr(IrExpr::GetValue(0));
    let text = ir.add_expr(IrExpr::Const(IrConst::String("a".into())));
    let number = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let boxed = ir.add_expr(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: number,
        type_operand: Ty::obj("kotlin/Any"),
    });
    let branches = vec![(Some(condition), text), (None, boxed)];
    let when = match result {
        Some(result) => ir.add_when(branches, result),
        None => ir.add_expr(IrExpr::When { branches }),
    };
    let returned = ir.add_expr(IrExpr::Return(Some(when)));
    ir.add_fun(IrFunction {
        name: "box".into(),
        params: vec![Ty::Boolean],
        ret: Ty::obj("kotlin/Any"),
        body: Some(returned),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    });
    ir
}

#[test]
fn a_value_when_is_emitted_at_its_recorded_result() {
    let run = EmitRun::default();
    let classes = emit_for_test(
        &conditional_function(Some(Ty::obj("kotlin/Any"))),
        "TestKt",
        &run,
    )
    .expect("emitted class");
    assert_eq!(run.emit_error(), None);
    assert!(classes.iter().any(|(name, _)| name == "TestKt"));
}

#[test]
fn a_value_when_without_a_recorded_result_is_rejected() {
    let run = EmitRun::default();
    assert!(
        emit_for_test(&conditional_function(None), "TestKt", &run).is_none(),
        "emission must not join the branches of an unrecorded `when`"
    );
    assert_eq!(
        run.emit_error().as_deref(),
        Some("a value `when` has no recorded result type"),
    );
}
