//! A NESTED lambda capturing a variable from 2+ levels out (`host { inner { x = outer } }`) lowered to
//! `lower None` (skipped): `lower_lambda_sam`'s capture detection stopped at a nested lambda, so the
//! outer closure never captured the transitively-used variable. Now a CLOSURE lambda captures through
//! nested lambdas — while an INLINE-spliced lambda keeps shallow captures (it accesses the variable
//! directly). Round-tripped on a real JVM.

mod common;

#[test]
fn nested_closure_capture_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    // `host`/`inner` are NON-inline (real closures); the inner lambda captures `outer` two levels out.
    const SRC: &str = "fun host(b: () -> Unit) { b() }\n\
        fun inner(f: () -> Unit) { f() }\n\
        fun box(): String {\n\
        \x20 var x = \"\"\n\
        \x20 val outer = \"OK\"\n\
        \x20 host { inner { x = outer } }\n\
        \x20 return x\n\
        }\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(SRC, "N", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
