//! A function type is a subtype of its `FunctionN` classifier and of `Function<R>`, so a call
//! whose parameter is `Function<R>` infers `R` from a function value's result type, as kotlinc does.

use super::common;

const SOURCE: &str = r#"
fun <R> accepts(f: Function<R>): Function<R> = f
fun <R> withKind(f: Function<R>, kind: Int): Int = kind

fun box(): String {
    val unit: () -> Unit = {}
    val text: (Int) -> String = { "OK" }
    if (accepts(unit) !== unit) return "unit"
    if (withKind(text, 2) != 2) return "kind"
    val kept: Function<String> = accepts(text)
    return if (kept === text) "OK" else "text"
}
"#;

#[test]
fn function_values_bind_the_function_result() {
    common::assert_accepted_like_kotlinc(SOURCE);
    assert_eq!(
        common::expect_box_run_with_stdlib(SOURCE, "FunctionSupertypeInference"),
        "OK"
    );
}
