//! A `@JvmInline value class` used as a hash-container key/element: the synthesized boxed wrapper's
//! `equals`/`hashCode` make map lookup, `HashMap.put`/`get`, `HashSet` membership, and `Map.keys`
//! iteration behave by underlying value. Round-tripped on the JVM via `compile_and_run_box`.
//!
//! (Value-class through a generic HOF key selector such as `groupBy { it }` is a separate, still-open
//! generic-boundary boxing case and is intentionally not covered here.)

use super::common;

fn run_ok(stem: &str, decls: &str, body: &str) {
    let src = format!("{decls}\nfun box(): String {{\n{body}\n}}\n");
    common::expect_box_ok_with_stdlib(&src, stem);
}

#[test]
fn map_of_with_int_backed_key() {
    run_ok(
        "VcKeyInt",
        "@JvmInline\nvalue class Key(val x: Int)",
        "val m = mapOf(Key(1) to \"a\", Key(2) to \"b\")\n\
         return if (m[Key(1)] == \"a\" && m[Key(2)] == \"b\" && m[Key(3)] == null) \"OK\" else \"F\"",
    );
}

#[test]
fn map_of_with_string_backed_key() {
    run_ok(
        "VcKeyStr",
        "@JvmInline\nvalue class Name(val s: String)",
        "val m = mapOf(Name(\"a\") to 1, Name(\"b\") to 2)\n\
         return if (m[Name(\"a\")] == 1 && m[Name(\"b\")] == 2) \"OK\" else \"F\"",
    );
}

#[test]
fn hashmap_put_get() {
    run_ok(
        "VcKeyHM",
        "@JvmInline\nvalue class Key(val x: Int)",
        "val m = HashMap<Key, Int>()\nm[Key(3)] = 30\nm[Key(4)] = 40\n\
         return if (m[Key(3)] == 30 && m[Key(4)] == 40) \"OK\" else \"F\"",
    );
}

#[test]
fn hashset_membership_dedup() {
    run_ok(
        "VcKeyHS",
        "@JvmInline\nvalue class Key(val x: Int)",
        "val hs = HashSet<Key>()\nhs.add(Key(5)); hs.add(Key(5)); hs.add(Key(6))\n\
         return if (hs.size == 2 && hs.contains(Key(5)) && Key(6) in hs) \"OK\" else \"F\"",
    );
}

#[test]
fn map_keys_iteration_and_value_class_value() {
    run_ok(
        "VcKeyIter",
        "@JvmInline\nvalue class Key(val x: Int)\n@JvmInline\nvalue class V(val n: Int)",
        "val m = mapOf(Key(1) to V(10), Key(2) to V(20))\nvar s = 0\nfor (k in m.keys) s += k.x\n\
         return if (s == 3 && m[Key(2)]?.n == 20) \"OK\" else \"F\"",
    );
}

#[test]
fn value_class_in_list_contains() {
    run_ok(
        "VcList",
        "@JvmInline\nvalue class K(val x: Int)",
        "val l = listOf(K(1), K(2), K(3))\n\
         return if (l.contains(K(2)) && K(1) in l && !l.contains(K(9))) \"OK\" else \"F\"",
    );
}
