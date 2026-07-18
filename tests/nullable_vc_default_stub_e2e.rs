//! A `@JvmInline value class` with a NULLABLE underlying (`VC(val s: String?)`) stays BOXED in a
//! `$default` synthetic (kotlinc — a `$default` can't disambiguate the unboxed signature without the
//! `-<hash>` mangling the base method carries). `emit_default_stub` takes the value class, `box-impl`s
//! any default-filled field value, and `unbox-impl`s before delegating; the CALL site boxes a provided
//! value-class arg to match. This drives `copy(...)` with an omitted value-class argument end-to-end.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn copy_default_boxes_nullable_underlying_value_class() {
    const SRC: &str = "@JvmInline value class VC(val s: String?)\n\
        data class D(val vc: VC, val n: Int)\n\
        fun box(): String {\n\
        \x20 val d = D(VC(\"a\"), 1)\n\
        \x20 val e = d.copy(n = 2)\n\
        \x20 val f = d.copy(vc = VC(\"b\"))\n\
        \x20 return if (e.vc.s == \"a\" && e.n == 2 && f.vc.s == \"b\" && f.n == 1) \"OK\"\n\
        \x20   else \"FAIL:${e.vc.s}|${e.n}|${f.vc.s}|${f.n}\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("nullable-underlying value-class copy$default"),
        "OK"
    );
}
