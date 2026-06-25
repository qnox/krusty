//! A nullable PARENTHESIZED function type — `(() -> Unit)?` / `((Int) -> Boolean)?` — is a function type
//! grouped in parens and made nullable (kotlinc accepts it; krusty's parser used to read the parens as a
//! function-type parameter list and demand a `->`). Parses, type-checks, and runs on a real JVM: the
//! defaulted-null parameter and a lambda argument both behave correctly.

mod common;

#[test]
fn nullable_parenthesized_function_type_param_compiles_and_runs() {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let java_home = std::env::var("JAVA_HOME").ok().filter(|v| !v.is_empty());
    let jdk = java_home
        .as_ref()
        .map(|jh| std::path::PathBuf::from(format!("{jh}/lib/modules")));

    // `block: (() -> Unit)? = null` — a nullable function-type parameter with a default.
    let src = "fun run0(block: (() -> Unit)? = null): Int = if (block == null) 0 else 1\n\
        fun box(): String =\n\
        \x20   if (run0() == 0 && run0({}) == 1) \"OK\" else \"fail:\" + run0() + \",\" + run0({})\n";
    let cp = std::slice::from_ref(&stdlib);
    let classes = common::compile_in_process(src, "Main", cp, jdk.as_deref())
        .expect("krusty failed to compile a nullable parenthesized function-type parameter");

    let Some(out) = common::run_box(&classes, "MainKt", cp) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");
}
