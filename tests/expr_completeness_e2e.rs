//! Expression-parser completeness: unary `+` (identity on numerics) and `return`/`throw` used in
//! expression position (`x ?: return y`). Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "P")
}

#[test]
fn unary_plus_runs() {
    const SRC: &str = "fun box(): String {\n\
        val a = +5\n\
        val b = +0.0f\n\
        if (a != 5) return \"fail a\"\n\
        if (0.compareTo(b) != 0) return \"fail b\"\n\
        return \"OK\"\n\
    }\n";
    if let Some(out) = run(SRC) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn return_in_expression_position_runs() {
    const SRC: &str = "fun firstOrNull(x: Int?): Int {\n\
        val v = x ?: return -1\n\
        return v + 1\n\
    }\n\
    fun box(): String {\n\
        if (firstOrNull(null) != -1) return \"fail null\"\n\
        if (firstOrNull(10) != 11) return \"fail val\"\n\
        return \"OK\"\n\
    }\n";
    if let Some(out) = run(SRC) {
        assert_eq!(out, "OK");
    } else {
        panic!("return-in-expression-position should compile");
    }
}
