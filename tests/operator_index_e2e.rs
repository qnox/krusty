//! User-class indexed access via operator overloads: `m[i]` → `m.get(i)`, `m[i] = v` → `m.set(i, v)`.
//! The checker resolves the index against the class's `get`/`set` member; the lowering emits the
//! corresponding instance method call (the same `invokevirtual` kotlinc emits). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_kotlinc_accepts(tag: &str, source: &str) {
    let (code, diagnostics) = common::kotlinc_source_result(tag, source);
    assert_eq!(code, 0, "kotlinc rejected {tag}: {diagnostics}");
}

fn assert_kotlinc_rejects(tag: &str, source: &str) {
    let (code, _) = common::kotlinc_source_result(tag, source);
    assert_ne!(code, 0, "kotlinc accepted {tag}");
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

#[test]
fn function_value_is_a_function_classifier_map_key() {
    const SRC: &str = "import java.util.concurrent.ConcurrentHashMap\n\
fun box(): String {\n\
    val f: () -> String = { \"OK\" }\n\
    val concurrent = ConcurrentHashMap<Function0<*>, Any>()\n\
    concurrent[f] = f()\n\
    val exact = mutableMapOf<Function0<String>, Any>()\n\
    exact[f] = f()\n\
    val star = mutableMapOf<Function0<*>, Any>()\n\
    star[f] = f()\n\
    val read = concurrent[f] as String\n\
    return if (read == \"OK\" && exact[f] == \"OK\" && star[f] == \"OK\") \"OK\" else \"no\"\n\
}\n";
    assert_kotlinc_accepts("FunctionClassifierMapKey", SRC);
    assert_eq!(
        run(SRC).expect("function-typed map key compiles + runs"),
        "OK"
    );
}

#[test]
fn function_value_is_not_a_different_function_classifier() {
    const SRC: &str = "fun box(): String {\n\
    val f: () -> String = { \"OK\" }\n\
    val wrong = mutableMapOf<Function0<Int>, Any>()\n\
    wrong[f] = f()\n\
    return \"OK\"\n\
}\n";
    assert_kotlinc_rejects("FunctionClassifierMapKeyMismatch", SRC);
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(SRC, &[stdlib], Some(jdk.as_path()));
    assert_eq!(
        diagnostics,
        ["'MutableMap<Function0<Int>, Any>' is not an array (cannot index-assign)"]
    );
}
