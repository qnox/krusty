//! A destructuring lambda parameter inside a SAFE-call scope function (`entry?.let { (k, v) -> … }`,
//! e.g. `map.entries.find { … }?.let { (name, cfg) -> cfg.field }`) must bind its components against the
//! NON-null receiver. The safe-scope path un-nullabled only a PRIMITIVE receiver, so a nullable reference
//! receiver (`Map.Entry<K,V>?`) reached the destructure as `Nullable(Map.Entry<…>)` — and `componentN`
//! resolution against a nullable type failed ("cannot destructure … no operator 'component1'"), which
//! then erased the lambda body's inferred type to `Any`. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn safe_let_destructures_map_entry() {
    const SRC: &str = "data class Cfg(val v: String)\n\
fun lookup(m: Map<String, Cfg>, id: String): String =\n\
    m.entries.find { (name, _) -> name == id }?.let { (name, cfg) -> \"$name:${cfg.v}\" } ?: \"none\"\n\
fun box(): String {\n\
    val m = mapOf(\"a\" to Cfg(\"x\"))\n\
    return if (lookup(m, \"a\") == \"a:x\" && lookup(m, \"z\") == \"none\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("safe ?.let destructuring a Map.Entry compiles + runs"),
        "OK"
    );
}

#[test]
fn safe_let_destructure_result_flows_to_inferred_return() {
    // The `?.let { (k, v) -> … } ?: throw` shape whose enclosing function has an INFERRED return type —
    // the destructure must resolve so the return type is the branch value, not `Any`.
    const SRC: &str = "data class Cfg(val v: Int)\n\
fun req(m: Map<String, Cfg>, id: String) =\n\
    m.entries.find { (name, _) -> name == id }?.let { (name, cfg) -> cfg }\n\
        ?: throw IllegalArgumentException(id)\n\
fun box(): String = if (req(mapOf(\"a\" to Cfg(7)), \"a\").v == 7) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("safe ?.let destructure result flows to inferred return"),
        "OK"
    );
}
