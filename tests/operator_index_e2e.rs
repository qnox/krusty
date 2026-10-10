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
    let result = common::compiler_diagnostics(&[("Main.kt", SRC)], &[]);
    assert_eq!(result.krusty_code, 1);
    assert_eq!(result.reference_code, 1);
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 4,
            column: 1,
            message: "'MutableMap<Function0<Int>, Any>' is not an array (cannot index-assign)"
                .to_string(),
        }]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 4,
            column: 7,
            message:
                "argument type mismatch: actual type is '() -> String', but '() -> Int' was expected."
                    .to_string(),
        }]
    );
}

/// A dependency's generic `get` operator extension read through a star-projected receiver:
/// `operator fun <K, V> Table<out K, V>.get(key: K): V?` on `Table<*, *>`. The receiver alone binds
/// `V` (to the star's `Any?`), exactly as the equivalent `t.get(key)` call does. The subscript form
/// published no solved type arguments, so an expectation-free `val local = table["a"]` read its
/// `Any?` result as the unsolved fallback and reported "cannot infer type for type parameter 'V'".
/// The stdlib `Map<*, *>` subscript is the same extension shape.
const STAR_PROJECTED_LIB: &str = "package lib\n\
    class Table<K, V>(private val keys: List<K>, private val values: List<V>) {\n\
        fun find(key: Any?): V? = keys.indexOf(key).let { if (it < 0) null else values[it] }\n\
    }\n\
    operator fun <K, V> Table<out K, V>.get(key: K): V? = find(key)\n";

const STAR_PROJECTED_MAIN: &str = "import lib.Table\n\
    import lib.get\n\
    fun cell(table: Table<*, *>): Any? = table[\"b\"]\n\
    fun entry(map: Map<*, *>): Any? = map[\"b\"]\n\
    fun box(): String {\n\
        val table: Table<*, *> = Table(listOf(\"a\", \"b\"), listOf(1, 2))\n\
        val local = table[\"a\"]\n\
        val map: Map<*, *> = mapOf(\"b\" to \"K\")\n\
        return if (cell(table) == 2 && local == 1 && entry(map) == \"K\") \"OK\" else \"FAIL\"\n\
    }\n";

#[test]
fn a_star_projected_receiver_binds_an_indexed_extension_result() {
    let Some(library) = common::kotlinc_library(STAR_PROJECTED_LIB) else {
        return;
    };
    let result = common::compiler_diagnostics(
        &[("Main.kt", STAR_PROJECTED_MAIN)],
        &[library, common::stdlib_jar()],
    );
    assert_eq!(
        result.reference_code, 0,
        "kotlinc must accept the fixture: {}",
        result.reference_stderr
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), []);
    assert_eq!(
        result.krusty_code, 0,
        "krusty rejected the fixture: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), []);
    let Some(boxed) =
        common::expect_box_run_against_kotlinc(STAR_PROJECTED_LIB, STAR_PROJECTED_MAIN)
    else {
        return;
    };
    assert_eq!(boxed, "OK");
}
