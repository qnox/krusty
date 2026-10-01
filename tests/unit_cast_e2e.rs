//! Casts involving `Unit` — which is the reference type `kotlin/Unit` at the JVM. A `Unit`-returning
//! expression used as a cast operand yields the `Unit.INSTANCE` singleton; `Unit` as a cast target is
//! `kotlin/Unit`. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unit_value_as_any() {
    const SRC: &str = "fun p(s: String) {}\n\
fun box(): String {\n\
    val x = p(\"hi\") as Any\n\
    return if (x == Unit) \"OK\" else \"fail: $x\"\n\
}\n";
    assert_eq!(run(SRC).expect("unit as Any"), "OK");
}

#[test]
fn unit_returning_call_as_unit() {
    const SRC: &str = "fun foo() {}\n\
fun box(): String {\n\
    foo() as Unit\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("foo() as Unit"), "OK");
}

#[test]
fn unit_safe_cast_to_primitive_is_null() {
    const SRC: &str = "fun foo() {}\n\
fun bar(): Int? = foo() as? Int\n\
fun box(): String = if (bar() == null) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("foo() as? Int is null"), "OK");
}

fn agrees(src: &str) -> String {
    let krusty = run(src).expect("krusty run");
    let reference = common::kotlinc_box_result(src);
    assert_eq!(reference, "OK", "unexpected kotlinc result: {reference}");
    assert_eq!(krusty, reference, "krusty and kotlinc disagree: {krusty}");
    krusty
}

/// `"str" as K` with `K` instantiated to `Unit` is a coercion to the `Unit` singleton.
#[test]
fn a_string_cast_to_a_unit_type_argument_is_unit() {
    const SRC: &str = "fun <K> materialize(): K = \"str\" as K\n\
fun box(): String {\n\
    val value: Unit = materialize()\n\
    return if (value == Unit) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(agrees(SRC), "OK");
}

/// A bare `return@label` makes the lambda `Unit`, so the last expression's type arguments are
/// `Unit` and its value is discarded.
#[test]
fn a_unit_lambda_coerces_its_last_generic_expression() {
    const SRC: &str = "fun <T> myRun(action: () -> T): T = action()\n\
fun foo(): String = \"foo\"\n\
fun <K> materialize(): K {\n\
    result += \"K\"\n\
    return \"str\" as K\n\
}\n\
var result = \"fail\"\n\
fun test1(n: Number, b: Boolean) {\n\
    n.let {\n\
        if (b) return@let\n\
        myRun {\n\
            result = \"O\"\n\
            foo()\n\
        }\n\
    }\n\
}\n\
fun test2(n: Number, b: Boolean) {\n\
    n.let {\n\
        if (b) return@let\n\
        materialize()\n\
    }\n\
}\n\
fun box(): String {\n\
    test1(42, false)\n\
    test2(42, false)\n\
    return result\n\
}\n";
    assert_eq!(agrees(SRC), "OK");
}

/// The recorded call, not only the runtime result, is the `Unit` instantiation of the generic tail.
#[test]
fn a_unit_lambda_records_the_generic_tail_as_unit() {
    const SRC: &str = "fun <K> materialize(): K = \"str\" as K\n\
fun box(): String {\n\
    val value = 1.let {\n\
        if (false) return@let\n\
        materialize()\n\
    }\n\
    return if (value == Unit) \"OK\" else \"fail\"\n\
}\n";
    let (errors, recorded) = common::inspect_checker_with_stdlib(SRC, |file, info, _| {
        file.expr_arena
            .iter()
            .enumerate()
            .find_map(|(index, expression)| {
                let krusty::ast::Expr::Call { callee, .. } = expression else {
                    return None;
                };
                let krusty::ast::Expr::Name(name) = file.expr(*callee) else {
                    return None;
                };
                if name != "materialize" {
                    return None;
                }
                let id = krusty::ast::ExprId(index as u32);
                Some((
                    info.expr_types.get(index).copied(),
                    info.resolved_call_type_args.get(&id).cloned(),
                ))
            })
    });
    assert!(errors.is_empty(), "{errors:?}");
    let (ty, args) = recorded.expect("materialize call");
    assert_eq!(ty, Some(krusty::types::Ty::Unit));
    assert_eq!(args, Some(vec![Some(krusty::types::Ty::Unit)]));
}

/// A local `(T) -> R` is substituted to `(T) -> Any` before the lambda is checked. That upper
/// bound is not a fixed result: the lambda's result and the recorded tail stay `Unit`.
#[test]
fn a_substituted_result_bound_still_records_the_generic_tail_as_unit() {
    const SRC: &str = "fun <T, R> T.myLet(block: (T) -> R): R = block(this)\n\
fun <K> materialize(): K = \"str\" as K\n\
fun box(): String {\n\
    val value = 1.myLet label@{\n\
        if (false) return@label\n\
        materialize()\n\
    }\n\
    return if (value == Unit) \"OK\" else \"fail\"\n\
}\n";
    let (errors, recorded) = common::inspect_checker_with_stdlib(SRC, |file, info, _| {
        let call = file
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(index, expression)| {
                let krusty::ast::Expr::Call { callee, .. } = expression else {
                    return None;
                };
                let krusty::ast::Expr::Name(name) = file.expr(*callee) else {
                    return None;
                };
                if name != "materialize" {
                    return None;
                }
                let id = krusty::ast::ExprId(index as u32);
                Some((
                    info.expr_types.get(index).copied(),
                    info.resolved_call_type_args.get(&id).cloned(),
                ))
            });
        let lambda_ret = file.lambda_labels.iter().find_map(|(id, label)| {
            (label == "label").then(|| {
                info.expr_types
                    .get(*id as usize)
                    .copied()
                    .and_then(krusty::types::Ty::fun_ret)
            })
        });
        (call, lambda_ret)
    });
    assert!(errors.is_empty(), "{errors:?}");
    let (call, lambda_ret) = recorded;
    let (ty, args) = call.expect("materialize call");
    assert_eq!(ty, Some(krusty::types::Ty::Unit));
    assert_eq!(args, Some(vec![Some(krusty::types::Ty::Unit)]));
    assert_eq!(lambda_ret, Some(Some(krusty::types::Ty::Unit)));
}

/// A source-declared `Any` result is a fixed expectation, even though it has the same type as the
/// upper bound used for an unconstrained call result. A valueless exit cannot satisfy that fixed
/// result merely because `Unit` is otherwise assignable to `Any`.
#[test]
fn a_fixed_any_lambda_result_rejects_a_valueless_exit() {
    const SRC: &str = "fun <K> materialize(): K = \"str\" as K\n\
fun box(): String {\n\
    val block: () -> Any = label@{\n\
        if (false) return@label\n\
        materialize()\n\
    }\n\
    return if (block() == \"str\") \"OK\" else \"fail\"\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", SRC)], &[]);
    let expected = vec![common::CompilerError {
        file: "Main.kt".to_string(),
        line: 4,
        column: 12,
        message: "return type mismatch: expected 'Any', actual 'Unit'.".to_string(),
    }];
    assert_eq!(result.reference_code, 1, "{}", result.reference_stderr);
    assert_eq!(result.krusty_code, 1, "{}", result.krusty_stderr);
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        expected,
        "complete ordered kotlinc diagnostics"
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected, "complete ordered krusty diagnostics");
}

/// The trailing `when` stays in statement position, so the missing `else` is legal and the lambda
/// result is `Unit`.
#[test]
fn a_unit_lambda_keeps_a_trailing_when_in_statement_position() {
    const SRC: &str = "fun box(): String {\n\
    val value = 1.let {\n\
        if (false) return@let\n\
        when (1) {\n\
            0 -> \"NO\"\n\
        }\n\
    }\n\
    return if (value == Unit) \"OK\" else \"value=$value\"\n\
}\n";
    assert_eq!(agrees(SRC), "OK");
}

/// Two lambdas spelled `label` are different targets. The valueless return binds to the inner one,
/// so the outer tail stays a value.
#[test]
fn a_same_label_inner_return_does_not_coerce_the_outer_lambda() {
    const SRC: &str = "fun box(): String {\n\
    var marker = \"fail\"\n\
    val outer = run label@{\n\
        val inner = run label@{\n\
            if (true) return@label\n\
            \"NO\"\n\
        }\n\
        marker = if (inner == Unit) \"OK\" else \"inner\"\n\
        \"TAIL\"\n\
    }\n\
    return if (outer == \"TAIL\" && marker == \"OK\") \"OK\" else \"outer=$outer marker=$marker\"\n\
}\n";
    assert_eq!(agrees(SRC), "OK");
}

/// `return@outer` written inside the inner literal is the outer lambda's exit. The inner literal
/// still returns its own tail, so that tail stays `String` while the outer result is `Unit`.
#[test]
fn an_outer_return_inside_an_inner_lambda_coerces_only_the_outer() {
    const SRC: &str = "fun probe(leave: Boolean): String {\n\
    var marker = \"fail\"\n\
    val outer = run outer@{\n\
        val produced: String = run inner@{\n\
            if (leave) return@outer\n\
            \"str\"\n\
        }\n\
        marker = produced\n\
        \"TAIL\"\n\
    }\n\
    return if (outer == Unit && marker == \"str\") \"OK\" else \"outer=$outer marker=$marker\"\n\
}\n\
fun box() = probe(false)\n";
    assert_eq!(agrees(SRC), "OK");
}

#[test]
fn primitive_safe_cast_to_unit_is_null() {
    const SRC: &str = "fun box(): String = if (4 as? Unit != null) \"fail\" else \"OK\"\n";
    assert_eq!(run(SRC).expect("4 as? Unit is null"), "OK");
}
