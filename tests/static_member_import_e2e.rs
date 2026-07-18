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

#[test]
fn classpath_varargs_static_qualified_and_via_import() {
    // A trailing VARARGS parameter on a classpath static (`java.util.Arrays.asList(T...)`) matched
    // element-wise, collecting the args into an array — BOTH qualified (`Arrays.asList(1, 2, 3)`) and
    // through a static member import (`import Arrays.asList; asList(...)`). Pervasive in the Mongo repos
    // (`Filters.and(Bson...)`, `Sorts.descending(String...)`). krusty resolved neither before, and once
    // resolving would emit N loose args against a 1-array-param descriptor (VerifyError) without the
    // call-site array collection.
    const SRC: &str = "import java.util.Arrays\n\
        import java.util.Arrays.asList\n\
        fun box(): String {\n\
        \x20 val a = Arrays.asList(1, 2, 3)\n\
        \x20 val b = asList(\"x\", \"y\")\n\
        \x20 return if (a.size == 3 && a[2] == 3 && b.size == 2 && b[0] == \"x\") \"OK\" else \"FAIL:$a|$b\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("classpath varargs static"),
        "OK"
    );
}

#[test]
fn classpath_object_jvmfield_and_java_static_field_read() {
    // A PUBLIC STATIC FIELD read `Type.name` that is neither a property getter nor an instance member —
    // a Kotlin `@JvmField` on an `object` (`Charsets.UTF_8`) or a Java static field — resolves to a
    // `getstatic <owner>.<name>`. krusty previously reported "unresolved member 'UTF_8' on
    // 'kotlin/text/Charsets'". Pervasive in the mission-infrastructure token/crypto code.
    const SRC: &str = "fun box(): String {\n\
        \x20 val cs = Charsets.UTF_8\n\
        \x20 return if (cs.name() == \"UTF-8\") \"OK\" else \"FAIL:${cs.name()}\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("static field read"),
        "OK"
    );
}
