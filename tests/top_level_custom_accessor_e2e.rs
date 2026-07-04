//! Top-level properties with a backing field AND a custom accessor
//! (`val x = init get() = field…`, `var y = init set(v) { field = … }`).
//! The backing field is a facade static; the synthesized `getX`/`setX` run the custom body
//! (with `field` bound to that static), and reads/writes route through those accessors.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn top_level_custom_getter_reads_field() {
    // A custom getter reading `field` whose result depends on a mutable flag — a read MUST route through
    // `getArr()` (not a direct field read), else `b` would still be "small".
    const SRC: &str = "var needBig = false\n\
val arr: String = \"small\"\n\
    get() = if (needBig) \"BIG\" else field\n\
fun box(): String {\n\
    val a = arr\n\
    needBig = true\n\
    val b = arr\n\
    return if (a == \"small\" && b == \"BIG\") \"OK\" else \"FAIL:$a,$b\"\n\
}\n";
    assert_eq!(run(SRC).expect("custom getter compiles + runs"), "OK");
}

#[test]
fn top_level_simple_field_getter() {
    // The minimal corpus shape (`val x = init get() = field`).
    const SRC: &str = "val x: String = \"OK\"\n    get() = field\nfun box() = x\n";
    assert_eq!(run(SRC).expect("simple field getter compiles + runs"), "OK");
}

#[test]
fn top_level_compound_assign_routes_through_accessors() {
    // `v += 2` desugars to `v = v + 2` — the read MUST call `getV()` and the write `setV()`, so the
    // custom setter's side effect (appending to `log`) runs on every compound assignment.
    const SRC: &str = "var log = \"\"\n\
var v: Int = 1\n\
    set(value) { log = log + \"[\" + value.toString() + \"]\"; field = value }\n\
fun box(): String {\n\
    v += 2\n\
    v += 4\n\
    return if (v == 7 && log == \"[3][7]\") \"OK\" else \"FAIL:$v,$log\"\n\
}\n";
    assert_eq!(run(SRC).expect("compound assign compiles + runs"), "OK");
}

#[test]
fn top_level_custom_setter_writes_field() {
    // A custom setter writing `field` plus a side effect — a write MUST route through `setV()`.
    const SRC: &str = "var log = \"\"\n\
var v: Int = 0\n\
    set(value) { log = log + value.toString(); field = value }\n\
fun box(): String {\n\
    v = 3\n\
    v = 7\n\
    return if (v == 7 && log == \"37\") \"OK\" else \"FAIL:$v,$log\"\n\
}\n";
    assert_eq!(run(SRC).expect("custom setter compiles + runs"), "OK");
}
