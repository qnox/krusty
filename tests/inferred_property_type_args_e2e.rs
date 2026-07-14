//! A class PROPERTY whose type is INFERRED from a generic constructor call
//! (`val m = HashMap<String, W>()`) must keep its type arguments, exactly as an inferred local `val`
//! does. The signature phase inferred the property as the RAW type (`HashMap`, no `<String, W>`), so a
//! later `m[k]` erased its value to `Any` and a member access on it failed to resolve. Round-tripped on
//! a JVM: the property is read back through indexing and its element's member is used.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn inferred_generic_property_keeps_type_args() {
    // `store` is a field with an inferred `HashMap<String, W>` type; `store[k]` must yield `W?`, so
    // `?: W("d")` is a `W` and `.name` resolves — not `Any`.
    const SRC: &str = "data class W(val name: String)\n\
class Box {\n\
    private val store = HashMap<String, W>()\n\
    fun put(k: String, w: W) { store[k] = w }\n\
    fun name(k: String): String {\n\
        val w = store[k] ?: W(\"default\")\n\
        return w.name\n\
    }\n\
}\n\
fun box(): String {\n\
    val b = Box()\n\
    b.put(\"a\", W(\"alpha\"))\n\
    return if (b.name(\"a\") == \"alpha\" && b.name(\"z\") == \"default\") \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("inferred generic property compiles + runs"),
        "OK"
    );
}
