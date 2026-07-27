use super::common;

#[test]
fn inline_bounded_return_inferred_from_nullable_context() {
    const SRC: &str = "inline fun <T : Comparable<T>> mk(): T? = null\n\
        fun box(): String {\n\
            val s: String? = mk()\n\
            return if (s == null) \"OK\" else \"no\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "inline_bounded_return_nullable_ctx");
}

#[test]
fn inline_bounded_return_declines_unrelated_nullable_context() {
    const SRC: &str = "class Unrelated\n\
        inline fun <T : Comparable<T>> mk(): T? = null\n\
        fun box(): String {\n\
            val s: Unrelated? = mk()\n\
            return \"OK\"\n\
        }\n";
    let Some(diags) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(
        diags
            .iter()
            .any(|m| m.contains("mismatch") && m.contains("Unrelated")),
        "expected an initializer type-mismatch diagnostic for the unsatisfied bound, got: {diags:?}"
    );
}
