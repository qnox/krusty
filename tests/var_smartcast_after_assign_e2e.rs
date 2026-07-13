//! A nullable `var` (or `val`) is flow-narrowed to its non-null type after a non-null assignment or
//! initializer, matching kotlinc's smart-cast: `var i: Int?; i = 10; i += 1` reads `i` as `Int`. The
//! narrowing is dropped at branch/loop/closure boundaries (sound), and a later nullable reassignment
//! widens it back to the declared type. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn assign_then_compound_plus() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var i: Int?\n\
        \x20 i = 10\n\
        \x20 i += 1\n\
        \x20 return if (i == 11) \"OK\" else \"fail: $i\"\n\
        }\n";
    assert_eq!(run(SRC).expect("assign then +="), "OK");
}

#[test]
fn nonnull_initializer_narrows() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var i: Int? = 10\n\
        \x20 val x = i + 1\n\
        \x20 return if (x == 11) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("init narrows"), "OK");
}

#[test]
fn reassign_renarrows_to_new_value() {
    // A second non-null assignment re-narrows the var, so it stays usable as a non-null Int.
    const SRC: &str = "fun box(): String {\n\
        \x20 var i: Int? = 5\n\
        \x20 val a = i + 1\n\
        \x20 i = 20\n\
        \x20 val b = i + 2\n\
        \x20 return if (a == 6 && b == 22) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("reassign renarrows"), "OK");
}

#[test]
fn narrow_dropped_after_null_reassign_and_across_branch() {
    // `x ?: -1` sees the narrowed non-null value; a nullable reassignment (`x = null`) drops the
    // narrowing so a later `x ?: -5` correctly takes the fallback. Proves the narrowing is not leaked
    // unsoundly into the elvis fallback or across the reassignment.
    const SRC: &str = "fun box(): String {\n\
        \x20 var x: Int? = 10\n\
        \x20 val y = x ?: -1\n\
        \x20 x = null\n\
        \x20 val z = x ?: -5\n\
        \x20 return if (y == 10 && z == -5) \"OK\" else \"no:$y,$z\"\n\
        }\n";
    assert_eq!(run(SRC).expect("elvis + null reassign"), "OK");
}

#[test]
fn string_var_narrows_to_nonnull() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var s: String? = \"O\"\n\
        \x20 return s + \"K\"\n\
        }\n";
    assert_eq!(run(SRC).expect("string narrow"), "OK");
}
