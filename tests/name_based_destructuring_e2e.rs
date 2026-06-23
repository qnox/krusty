//! Name-based `[a, b]` destructuring (`// LANGUAGE: +NameBasedDestructuring`, kotlinc's
//! `-Xname-based-destructuring`). A drop-in must accept it ONLY when the feature is enabled and reject
//! it otherwise — matching `kotlinc`, which errors "the feature name based destructuring is
//! experimental" without the flag. Both `[a, b]` and `(a, b)` desugar to the same positional
//! `componentN` calls (proven byte-identical against kotlinc), so the compiled-and-run result is "OK".

use std::path::PathBuf;

mod common;

fn run(src: &str) -> Option<String> {
    let java_home = common::java_home()?;
    let stdlib = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    common::compile_and_run_box(src, "Nb", &[stdlib], Some(&jdk))
}

const FOR_AND_VAL: &str = r#"
// LANGUAGE: +NameBasedDestructuring
class C(val i: Int) {
    operator fun component1() = i + 1
    operator fun component2() = i + 2
}
fun box(): String {
    var s = ""
    val arr = arrayOf(C(0), C(1), C(2))
    for ([a, b] in arr) { s += "$a:$b;" }
    if (s != "1:2;2:3;3:4;") return "for: $s"
    val [x, y] = C(10)
    if (x != 11 || y != 12) return "val: $x,$y"
    return "OK"
}
"#;

#[test]
fn name_based_destructuring_runs_when_enabled() {
    if let Some(out) = run(FOR_AND_VAL) {
        assert_eq!(out, "OK");
    }
}

const VAR_CAPTURED: &str = r#"
// LANGUAGE: +NameBasedDestructuring
class A {
    operator fun component1() = 1
    operator fun component2() = 2
}
fun box(): String {
    var [a, b] = A()
    val local = { a = 3 }
    local()
    return if (a == 3 && b == 2) "OK" else "fail"
}
"#;

#[test]
fn var_component_captured_and_mutated_in_lambda() {
    // A `var` destructured component captured AND written by a closure must be boxed (`Ref`), so the
    // closure's write is visible to the outer read. Regression for the lower_destructure boxing fix.
    if let Some(out) = run(VAR_CAPTURED) {
        assert_eq!(out, "OK");
    }
}

#[test]
fn name_based_destructuring_rejected_without_flag() {
    // Same source WITHOUT the `// LANGUAGE:` directive: krusty must reject `[a, b]` (compile fails →
    // `None`), exactly as default-flags kotlinc does. A `Some` here would mean we wrongly accepted it.
    let src = FOR_AND_VAL.replace("// LANGUAGE: +NameBasedDestructuring\n", "");
    // Only meaningful when the toolchain is present (otherwise both branches skip).
    if common::java_home().is_some() && common::stdlib_jar().is_some() {
        assert!(
            common::compile_in_process(
                &src,
                "Nb",
                &[common::stdlib_jar().unwrap()],
                Some(&PathBuf::from(format!(
                    "{}/lib/modules",
                    common::java_home().unwrap()
                ))),
            )
            .is_none(),
            "krusty accepted `[a, b]` destructuring without +NameBasedDestructuring"
        );
    }
}
