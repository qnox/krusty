//! Two overloads of the same name, each an expression body with an INFERRED (unannotated) return of a
//! different type (`fun f(x: Int) = x + 1` : Int, `fun f(s: String) = s + "!"` : String). The inferred
//! returns are recorded per `(name, parameter types)`, so a call binds the right overload's return.
//! Before the fix the override map was keyed by name alone, so the second overload clobbered the first
//! and `f("hi")` was mis-typed as `Int` ("operator cannot be applied to Int and String").

use super::common;
use std::path::PathBuf;

fn overload_e2e_env(label: &str) -> Option<(PathBuf, PathBuf)> {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping {label}: set JAVA_HOME");
        return None;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping {label}: no kotlin-stdlib jar found");
        return None;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    Some((stdlib, jdk))
}

fn compile_overload_case(src: &str, label: &str, expect_msg: &str) -> Option<String> {
    let (stdlib, jdk) = overload_e2e_env(label)?;
    Some(
        common::compile_and_run_box(src, "F", &[stdlib], Some(&jdk))
            .unwrap_or_else(|| panic!("{expect_msg}")),
    )
}

#[test]
fn overloaded_inferred_returns_dont_clobber() {
    let src = "fun f(x: Int) = x + 1\n\
fun f(s: String) = s + \"!\"\n\
fun box(): String {\n\
if (f(1) != 2) return \"fa\"\n\
if (f(\"hi\") != \"hi!\") return \"fb\"\n\
if (f(\"hi\").length != 3) return \"fc\"\n\
return \"OK\"\n\
}\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_inferred_return_e2e",
        "krusty must keep overloaded inferred returns distinct (f(Int):Int, f(String):String)",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn overloaded_top_level_defaults_use_selected_decl() {
    let src = "fun choose(x: Int = 1): String = \"int:$x\"\n\
fun choose(s: String, suffix: String = \"K\"): String = s + suffix\n\
fun box(): String = choose(s = \"O\")\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_top_level_defaults_use_selected_decl",
        "lowering must use the checker-selected overloaded top-level declaration",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn overloaded_top_level_spread_uses_selected_decl() {
    let src = "fun choose(vararg x: Long): String = \"long\"\n\
fun choose(vararg x: Int): String = \"OK\"\n\
fun box(): String {\n\
    val parts = intArrayOf(1, 2)\n\
    return choose(*parts)\n\
}\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_top_level_spread_uses_selected_decl",
        "spread lowering must use the checker-selected overloaded top-level vararg",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn overloaded_top_level_inline_uses_selected_decl() {
    let src = "inline fun choose(x: Int): String = \"int\"\n\
inline fun choose(s: String): String = s\n\
fun box(): String = choose(\"OK\")\n";
    let Some(out) = compile_overload_case(
        src,
        "overloaded_top_level_inline_uses_selected_decl",
        "inline lowering must use the checker-selected overloaded top-level declaration",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn selected_overload_return_not_refined_by_generic_name_match() {
    let src = "fun <T> choose(x: T): T = x\n\
fun choose(x: String): Int = 42\n\
fun box(): String = if (choose(\"x\") == 42) \"OK\" else \"fail\"\n";
    let Some(out) = compile_overload_case(
        src,
        "selected_overload_return_not_refined_by_generic_name_match",
        "return refinement must use the checker-selected top-level overload",
    ) else {
        return;
    };
    assert_eq!(out, "OK");
}
