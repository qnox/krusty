//! An unqualified call to a STATIC method imported via a member import — a Java class's static
//! (`import java.lang.Integer.parseInt; parseInt("42")`) or a Kotlin `@JvmStatic`/companion static.
//! krusty resolved a member import only when the owner was a Kotlin `object` (singleton dispatch); a
//! plain class's static went unresolved ("unresolved function"). It now resolves through the same
//! `resolve_companion` the qualified `Integer.parseInt(…)` path uses, emitting the `invokestatic`
//! without a receiver. Pervasive in the mission-infrastructure Mongo repos (`import Filters.eq`).
use super::common;

#[test]
fn java_static_method_imported_unqualified_resolves() {
    const SRC: &str = "import java.lang.Integer.parseInt\n\
        fun box(): String {\n\
        \x20 val n = parseInt(\"42\") + parseInt(\"8\")\n\
        \x20 return if (n == 50) \"OK\" else \"FAIL:$n\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("static-member import"),
        "OK"
    );
}
