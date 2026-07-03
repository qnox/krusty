//! Comparisons against the literal `0`, which the backend emits with the single-operand `iflt`/`ifge`/
//! `ifle`/`ifgt`/`ifeq`/`ifne` branches (`cmp0_branch`) rather than pushing a `0` and using `if_icmp*`.
//! The corpus exercises a couple; this walks all six relations with a runtime operand (a parameter, so
//! the comparison isn't const-folded) in both true and false outcomes.

mod common;

fn run_ok(stem: &str, body: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping compare_to_zero_branch_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping compare_to_zero_branch_e2e: no kotlin-stdlib jar");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(body, stem, &[stdlib], Some(&jdk)) else {
        panic!("{stem}: compile/run returned None");
    };
    assert_eq!(out, "OK", "{stem}");
}

#[test]
fn all_relations_against_zero() {
    run_ok(
        "Cmp0",
        "fun bits(n: Int): Int {\n\
         var r = 0\n\
         if (n < 0) r += 1\n\
         if (n > 0) r += 2\n\
         if (n <= 0) r += 4\n\
         if (n >= 0) r += 8\n\
         if (n == 0) r += 16\n\
         if (n != 0) r += 32\n\
         return r\n\
         }\n\
         fun box(): String {\n\
         if (bits(-5) != 1 + 4 + 32) return \"neg=${bits(-5)}\"\n\
         if (bits(5) != 2 + 8 + 32) return \"pos=${bits(5)}\"\n\
         if (bits(0) != 4 + 8 + 16) return \"zero=${bits(0)}\"\n\
         return \"OK\"\n\
         }\n",
    );
}
