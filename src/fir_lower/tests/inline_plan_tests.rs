use super::*;

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

    for name in [
        "applied",
        "alsoed",
        "letResult",
        "runResult",
        "topRun",
        "topWith",
    ] {
        let body = ir
            .functions
            .iter()
            .find(|function| function.name == name)
            .and_then(|function| function.body)
            .unwrap_or_else(|| panic!("missing lowered body for {name}"));
        assert!(expression_subtree_contains(&ir, body, &|_, expression| {
            matches!(expression, IrExpr::InvokeFunction { .. })
        }));
        assert!(!expression_subtree_contains(&ir, body, &|_, expression| {
            matches!(
                expression,
                IrExpr::Call {
                    callee: Callee::External { .. },
                    ..
                }
            )
        }));
    }
}
