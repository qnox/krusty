//! Destructuring a `Map.Entry` (`for ((k, v) in map.entries) { … }`) resolves `component1`/`component2`,
//! which are `@InlineOnly` stdlib extensions (they inline to `getKey()`/`getValue()`) — reachable only
//! through the inline-callable resolution path, in both the checker and the lowerer. The keys/values
//! are typed `Any` (the entry's type arguments are erased), so they are used here through `Any`-valid
//! operations (string templates, which call `toString`). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn for_destructure_over_map_entries() {
    const SRC: &str = "fun box(): String {\n\
        val m = linkedMapOf(\"O\" to \"K\")\n\
        var s = \"\"\n\
        for ((k, v) in m.entries) { s += \"$k$v\" }\n\
        return s\n\
    }\n";
    assert_eq!(
        run(SRC).expect("destructuring Map.Entry in a for-loop"),
        "OK"
    );
}

#[test]
fn destructure_single_map_entry() {
    const SRC: &str = "fun box(): String {\n\
        val m = linkedMapOf(\"O\" to \"K\")\n\
        val (k, v) = m.entries.first()\n\
        return \"$k$v\"\n\
    }\n";
    assert_eq!(run(SRC).expect("destructuring a single Map.Entry"), "OK");
}

#[test]
fn for_destructure_directly_over_map() {
    // `for ((k, v) in map)` (no `.entries`) — the `Map<K,V>.iterator()` `@InlineOnly` extension
    // inlines to `entries.iterator()`.
    const SRC: &str = "fun box(): String {\n\
        val m = linkedMapOf(\"O\" to \"K\")\n\
        var s = \"\"\n\
        for ((k, v) in m) { s += \"$k$v\" }\n\
        return s\n\
    }\n";
    assert_eq!(run(SRC).expect("destructuring directly over a Map"), "OK");
}

#[test]
fn discarded_map_put_does_not_unbox_null() {
    // `map.put(k, v)` returns the previous value (`V?`, null for a fresh key); as a discarded statement
    // its result must NOT be unboxed (an `Integer.intValue()` on null NPEs).
    const SRC: &str = "fun box(): String {\n\
        val m = HashMap<String, Int>()\n\
        m.put(\"a\", 1)\n\
        m.put(\"b\", 2)\n\
        return if (m.size == 2) \"OK\" else \"fail\"\n\
    }\n";
    assert_eq!(run(SRC).expect("discarded put doesn't unbox null"), "OK");
}
