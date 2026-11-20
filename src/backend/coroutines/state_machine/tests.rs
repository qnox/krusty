use super::*;
use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::ir::for_each_child;

fn runtime(result: Ty) -> crate::ir::IrCoroutineRuntime {
    crate::ir::IrCoroutineRuntime {
        suspended: ExternalPropertyId::from_raw(0),
        throw_on_failure: ExternalCallableId::from_raw(0),
        throw_on_failure_receiver: result,
        create_failure: ExternalCallableId::from_raw(3),
        result_constructor: ExternalCallableId::from_raw(4),
        result_value: ExternalPropertyId::from_raw(2),
        resume_with: ExternalCallableId::from_raw(1),
        resume_with_declared_parameter: result,
        resume_with_declared_result: Ty::Unit,
        context: ExternalPropertyId::from_raw(1),
        context_getter: ExternalCallableId::from_raw(2),
        context_ty: Ty::obj_name(type_name("kotlin/coroutines/CoroutineContext")),
        context_receiver_ty: Ty::obj_name(wk::continuation()),
    }
}

/// A generated `resumeWith` runs the machine in a `try` and then tests for the suspension marker.
/// Both are statements a backend lowers by their checked type, which no source checker recorded.
#[test]
fn a_generated_resume_types_its_conditional_and_try_statements() {
    let any = Ty::nullable(Ty::obj_name(wk::any()));
    let machine_result_ty = Ty::obj_args_name(type_name("kotlin/Result"), &[any]);
    let completion_result_ty = Ty::obj_args_name(type_name("kotlin/Result"), &[Ty::Int]);
    let mut ir = IrFile::default();
    let class = ir.add_class(IrClass::synthetic(type_name("machine/Continuation")));
    ir.classes[class as usize].fields = vec![
        IrField::new(
            "completion".to_string(),
            Ty::obj_args_name(wk::continuation(), &[Ty::Int]),
        ),
        IrField::new("result".to_string(), machine_result_ty),
    ];

    let resume = build_resume_with(
        &mut ir,
        ResumeContract {
            class,
            runtime: runtime(machine_result_ty),
            machine_result_ty,
            completion_result_ty,
            any,
            completion_field: 0,
            result_field: 1,
        },
    )
    .expect("a resumeWith over a completion field");

    let mut pending = vec![ir.functions[resume as usize]
        .body
        .expect("a generated body")];
    let mut statements = Vec::new();
    while let Some(node) = pending.pop() {
        if matches!(
            ir.exprs[node as usize],
            IrExpr::When { .. } | IrExpr::Try { .. }
        ) {
            statements.push(ir.checked_type(node));
        }
        for_each_child(&ir.exprs, node, &mut |child| pending.push(child));
    }
    assert_eq!(statements, [Some(Ty::Unit), Some(Ty::Unit)]);
}
