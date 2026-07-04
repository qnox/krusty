//! User-class indexed access via operator overloads: `m[i]` → `m.get(i)`, `m[i] = v` → `m.set(i, v)`.
//! The checker resolves the index against the class's `get`/`set` member; the lowering emits the
//! corresponding instance method call (the same `invokevirtual` kotlinc emits). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn operator_get() {
    const SRC: &str = "class M(val s: String) { operator fun get(i: Int): Char = s[i] }\n\
fun box(): String { val m = M(\"OK\"); return if (m[0] == 'O' && m[1] == 'K') \"OK\" else \"no\" }\n";
    assert_eq!(run(SRC).expect("operator get compiles + runs"), "OK");
}

#[test]
fn operator_get_and_set() {
    const SRC: &str = "class M {\n\
    var stored = \"x\"\n\
    operator fun get(i: Int): String = stored\n\
    operator fun set(i: Int, v: String) { stored = v }\n\
}\n\
fun box(): String { val m = M(); m[0] = \"OK\"; return m[0] }\n";
    assert_eq!(run(SRC).expect("operator get+set compiles + runs"), "OK");
}

#[test]
fn operator_get_string_key() {
    const SRC: &str = "class Env {\n\
    private val a = StringBuilder()\n\
    operator fun get(k: String): String = a.toString() + k\n\
}\n\
fun box(): String = if (Env()[\"OK\"] == \"OK\") \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("string-key operator get compiles + runs"),
        "OK"
    );
}
