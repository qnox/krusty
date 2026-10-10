use super::*;

#[derive(Debug, Eq, PartialEq)]
enum SelectedCall {
    FunctionValue { parameters: Vec<Ty>, result: Ty },
    Local(crate::ir::FunId),
    External(crate::fir::ExternalCallableId),
}

fn selected_call_ledger(ir: &IrFile, root: crate::ir::ExprId) -> Vec<SelectedCall> {
    fn visit(
        ir: &IrFile,
        expression: crate::ir::ExprId,
        seen: &mut std::collections::HashSet<crate::ir::ExprId>,
        ledger: &mut Vec<SelectedCall>,
    ) {
        if !seen.insert(expression) {
            return;
        }
        match ir.expr(expression) {
            IrExpr::InvokeFunction { params, ret, .. } => {
                ledger.push(SelectedCall::FunctionValue {
                    parameters: params.clone(),
                    result: *ret,
                });
            }
            IrExpr::Call {
                callee: Callee::Local(target),
                ..
            } => ledger.push(SelectedCall::Local(*target)),
            IrExpr::Call {
                callee: Callee::External { target, .. },
                ..
            } => ledger.push(SelectedCall::External(*target)),
            _ => {}
        }
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        for child in children {
            visit(ir, child, seen, ledger);
        }
    }

    let mut ledger = Vec::new();
    visit(ir, root, &mut std::collections::HashSet::new(), &mut ledger);
    ledger
}

#[test]
fn unused_foreign_inline_body_is_not_lowered_into_the_active_source() {
    let ir = lower_source_from_set(
        &[
            (
                "inline fun <reified T> unused(value: Any): T = value as T",
                "Library",
            ),
            ("fun use(): String = \"OK\"", "Consumer"),
        ],
        1,
    );

    assert!(ir.foreign_inline_templates.is_empty());
    assert!(ir
        .functions
        .iter()
        .all(|function| function.name != "unused"));
}

#[test]
fn dependency_scope_plans_invoke_function_values_before_backend_lowering() {
    let ir = lower_single_source_with_jvm_stdlib(
        "class Box\n\
         fun applied(x: Box, block: Box.() -> Unit): Box = x.apply(block)\n\
         fun alsoed(x: Box, block: (Box) -> Unit): Box = x.also(block)\n\
         fun letResult(x: Box, block: (Box) -> Int): Int = x.let(block)\n\
         fun runResult(x: Box, block: Box.() -> Int): Int = x.run(block)\n\
         fun topRun(block: () -> Int): Int = run(block)\n\
         fun topWith(x: Box, block: Box.() -> Int): Int = with(x, block)\n",
        "DependencyScopeFunctionValues",
    );

    for (name, parameters, result) in [
        ("applied", vec![Ty::obj("Box")], Ty::Unit),
        ("alsoed", vec![Ty::obj("Box")], Ty::Unit),
        ("letResult", vec![Ty::obj("Box")], Ty::Int),
        ("runResult", vec![Ty::obj("Box")], Ty::Int),
        ("topRun", vec![], Ty::Int),
        ("topWith", vec![Ty::obj("Box")], Ty::Int),
    ] {
        let body = ir
            .functions
            .iter()
            .find(|function| function.name == name)
            .and_then(|function| function.body)
            .unwrap_or_else(|| panic!("missing lowered body for {name}"));
        assert_eq!(
            selected_call_ledger(&ir, body),
            [SelectedCall::FunctionValue { parameters, result }],
            "selected call ledger for {name}"
        );
    }
}

#[test]
fn dependency_scope_plans_invoke_function_values_captured_by_local_classes() {
    let ir = lower_single_source_with_jvm_stdlib(
        "interface Sink { fun accept(value: String?) }\n\
         fun captured(block: (String) -> Unit): Sink = object : Sink {\n\
         \x20   override fun accept(value: String?) { value?.let(block) }\n\
         }\n\
         fun sharedCaptured(start: (String) -> Unit): Sink {\n\
         \x20   var block = start\n\
         \x20   val sink = object : Sink {\n\
         \x20       override fun accept(value: String?) { value?.let(block) }\n\
         \x20   }\n\
         \x20   block = start\n\
         \x20   return sink\n\
         }\n",
        "CapturedScopeFunctionValues",
    );

    let bodies = ir
        .functions
        .iter()
        .filter(|function| function.name == "accept")
        .filter_map(|function| function.body)
        .collect::<Vec<_>>();
    assert_eq!(bodies.len(), 2);
    for body in bodies {
        assert_eq!(
            selected_call_ledger(&ir, body),
            [SelectedCall::FunctionValue {
                parameters: vec![Ty::String],
                result: Ty::Unit,
            }]
        );
    }
}

#[test]
fn dependency_scope_plans_invoke_every_checked_function_expression() {
    let ir = lower_single_source_with_jvm_stdlib(
        "class Holder(val block: (String) -> Int)\n\
         fun id(value: String): Int = value.length\n\
         fun make(): (String) -> Int = ::id\n\
         fun property(holder: Holder): Int = \"value\".let(holder.block)\n\
         fun computed(): Int = \"value\".let(make())\n\
         fun reference(): Int = \"value\".let(::id)\n",
        "DependencyScopeFunctionExpressions",
    );

    let identity = ir
        .functions
        .iter()
        .position(|function| function.name == "id")
        .and_then(|function| crate::ir::FunId::try_from(function).ok())
        .expect("id function");
    let producer = ir
        .functions
        .iter()
        .position(|function| function.name == "make")
        .and_then(|function| crate::ir::FunId::try_from(function).ok())
        .expect("make function");
    for (name, expected) in [
        (
            "property",
            vec![SelectedCall::FunctionValue {
                parameters: vec![Ty::String],
                result: Ty::Int,
            }],
        ),
        (
            "computed",
            vec![
                SelectedCall::Local(producer),
                SelectedCall::FunctionValue {
                    parameters: vec![Ty::String],
                    result: Ty::Int,
                },
            ],
        ),
        ("reference", vec![SelectedCall::Local(identity)]),
    ] {
        let body = ir
            .functions
            .iter()
            .find(|function| function.name == name)
            .and_then(|function| function.body)
            .unwrap_or_else(|| panic!("missing lowered body for {name}"));
        assert_eq!(
            selected_call_ledger(&ir, body),
            expected,
            "selected call ledger for {name}"
        );
    }
}
