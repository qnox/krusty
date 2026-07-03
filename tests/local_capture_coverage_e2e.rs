//! Local functions and lambdas capturing enclosing locals/`this`, labeled returns, and try/finally in
//! captured bodies — exercises the parser/lowerer capture-analysis helpers (local_fun_body_uses_any,
//! lambda_uses_enclosing_this, collect_locals, body_declares_local, outer_local_access_expr,
//! body_has_labeled_return, expr_has_try/finally) that the box corpus does not reach.

mod common;

fn run_ok(stem: &str, body: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping local_capture_coverage_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping local_capture_coverage_e2e: no kotlin-stdlib jar");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(body, stem, &[stdlib], Some(&jdk)) else {
        panic!("{stem}: compile/run returned None");
    };
    assert_eq!(out, "OK", "{stem}");
}

#[test]
fn local_fun_captures_enclosing_val() {
    run_ok(
        "LFCap",
        "fun box(): String {\n\
         val base = 40\n\
         fun add(x: Int): Int = base + x\n\
         return if (add(2) == 42) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn local_fun_captures_multiple_and_local_decl() {
    run_ok(
        "LFMulti",
        "fun box(): String {\n\
         val a = 1; val b = 2\n\
         fun compute(): Int { val c = 3; return a + b + c }\n\
         return if (compute() == 6) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn lambda_captures_enclosing_this_in_member() {
    run_ok(
        "LamThis",
        "class Counter(val step: Int) {\n\
         fun run(): Int { val f = { step * 2 }; return f() }\n\
         }\n\
         fun box(): String { return if (Counter(21).run() == 42) \"OK\" else \"F\" }\n",
    );
}

#[test]
fn local_fun_with_try_finally_capturing() {
    run_ok(
        "LFTry",
        "fun box(): String {\n\
         val tag = \"x\"\n\
         val sb = StringBuilder()\n\
         fun work(): String { try { sb.append(tag); return sb.toString() } finally { sb.append(\"F\") } }\n\
         return if (work() == \"x\") \"OK\" else \"F\" }\n",
    );
}
