//! Backend member-name translations and constructor shapes the corpus underexercises: the
//! `Number.toByte/toShort/toInt/toLong` → `byteValue/…` mapping (called on a `Number`-typed value),
//! `Map.entries` → `entrySet`, and a secondary constructor delegating to the primary via `this(...)`.

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn number_conversion_methods() {
    run_ok(
        "NumConv",
        "fun box(): String {\n\
         val n: Number = 42\n\
         if (n.toByte().toInt() != 42) return \"b\"\n\
         if (n.toShort().toInt() != 42) return \"s\"\n\
         if (n.toInt() != 42) return \"i\"\n\
         if (n.toLong() != 42L) return \"l\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn map_entries_property() {
    run_ok(
        "MapEntries",
        "fun box(): String {\n\
         val m = mapOf(1 to \"a\", 2 to \"b\")\n\
         var sum = 0\n\
         for (e in m.entries) sum += e.key\n\
         return if (sum == 3) \"OK\" else \"sum=$sum\"\n\
         }\n",
    );
}

#[test]
fn secondary_constructor_delegation() {
    run_ok(
        "SecondaryCtor",
        "class C(val x: Int) {\n\
         constructor() : this(99)\n\
         }\n\
         fun box(): String {\n\
         val a = C(7)\n\
         val b = C()\n\
         return if (a.x == 7 && b.x == 99) \"OK\" else \"a=${a.x} b=${b.x}\"\n\
         }\n",
    );
}
