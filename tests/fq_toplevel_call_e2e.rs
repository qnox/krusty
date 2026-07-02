//! A FULLY-QUALIFIED top-level function call `a.b.helper(args)` against a classpath dependency — the
//! callee is a dotted path whose prefix (`a.b`) is a PACKAGE and whose last segment is a top-level
//! function of that package (compiled to `a/b/<File>Kt`). Before, the leftmost segment was typed as a
//! value → "unresolved reference 'a'". Now the checker resolves the top-level overload and confirms its
//! owning facade sits in the receiver's package; the lowerer emits the `invokestatic` to the facade.
//! Built by the real kotlinc via the shared `common::run_box_against` harness.
mod common;

const LIB: &str = "package a.b\n\
     fun helper(): Int = 42\n\
     fun scaled(n: Int): Int = n * 10\n\
     fun greet(name: String): String = \"hi \" + name\n";

#[test]
fn fully_qualified_top_level_call() {
    let main = "fun box(): String {\n\
        \x20 if (a.b.helper() != 42) return \"fail helper: ${a.b.helper()}\"\n\
        \x20 if (a.b.scaled(5) != 50) return \"fail scaled: ${a.b.scaled(5)}\"\n\
        \x20 if (a.b.greet(\"x\") != \"hi x\") return \"fail greet: ${a.b.greet(\"x\")}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) = common::run_box_against("fq_toplevel", LIB, main) {
        assert_eq!(out.trim(), "OK", "box() = {out:?}");
    }
}
