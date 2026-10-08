//! `mapOf(...)` / `setOf(...)` and what a program does with the map or set it answers.
//!
//! A map is two growable lists side by side — its keys in insertion order and the values beside
//! them — and a set is the same object with no values, which is what Kotlin's own `LinkedHashSet`
//! is. Lookup is LINEAR, by `equals`; Kotlin's is by hash, and the difference is speed and nothing
//! else. What a hash table would not give is the ORDER, and order is the observable part: `mapOf`
//! answers a `LinkedHashMap`, whose iteration, `toString` and `keys` are in insertion order.
//!
//! The unordered spellings — `hashMapOf`, `HashSet()` — answer this object too, because their
//! order is unspecified and insertion order is one of the orders left unspecified. A test may
//! therefore not pin what one of those prints, and none below does.
//!
//! `keys`, `values` and `entries` are SNAPSHOTS where Kotlin's are views, the same trade
//! `toList()` on an array makes.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// A read-only map: its size, its lookups, and the two questions about membership.
#[test]
fn a_map_answers_what_it_holds_and_what_it_does_not() {
    let source = "fun box(): String {\n\
         \x20   val m = mapOf(\"a\" to 1, \"b\" to 2)\n\
         \x20   if (m.size != 2) return \"fail size\"\n\
         \x20   if (m[\"a\"] != 1 || m[\"b\"] != 2) return \"fail get\"\n\
         \x20   if (m[\"z\"] != null) return \"fail absent\"\n\
         \x20   if (!m.containsKey(\"b\") || m.containsKey(\"z\")) return \"fail containsKey\"\n\
         \x20   if (!m.containsValue(2) || m.containsValue(9)) return \"fail containsValue\"\n\
         \x20   if (m.isEmpty()) return \"fail isEmpty\"\n\
         \x20   if (!emptyMap<String, Int>().isEmpty()) return \"fail empty\"\n\
         \x20   if (mapOf<String, Int>().size != 0) return \"fail no pairs\"\n\
         \x20   // The one-pair form Kotlin declares beside the vararg one.\n\
         \x20   if (mapOf(\"a\" to 1)[\"a\"] != 1) return \"fail one pair\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "MapReads");
    expect_native_box(source, "MapReads", "OK");
}

/// A growable map, and the position a re-put key keeps.
#[test]
fn a_mutable_map_is_written_through_and_keeps_a_rewritten_keys_place() {
    let source = "fun box(): String {\n\
         \x20   val m = mutableMapOf<String, Int>()\n\
         \x20   m[\"a\"] = 1\n\
         \x20   m.put(\"b\", 2)\n\
         \x20   if (m.size != 2) return \"fail size\"\n\
         \x20   if (m.put(\"a\", 9) != 1) return \"fail previous\"\n\
         \x20   if (m.size != 2 || m[\"a\"] != 9) return \"fail overwrite\"\n\
         \x20   // A `LinkedHashMap` does not move a key that is written again.\n\
         \x20   if (\"\" + m != \"{a=9, b=2}\") return \"fail order \" + m\n\
         \x20   if (m.remove(\"a\") != 9) return \"fail remove\"\n\
         \x20   if (m.size != 1 || m.containsKey(\"a\")) return \"fail removed\"\n\
         \x20   m.clear()\n\
         \x20   if (!m.isEmpty()) return \"fail clear\"\n\
         \x20   val h = HashMap<String, Int>()\n\
         \x20   h[\"x\"] = 7\n\
         \x20   if (h[\"x\"] != 7) return \"fail HashMap\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "MapWrites");
    expect_native_box(source, "MapWrites", "OK");
}

/// Iteration, in insertion order, over the map and over each of its three views.
#[test]
fn a_map_is_walked_in_the_order_its_entries_were_put() {
    let source = "fun box(): String {\n\
         \x20   val m = mapOf(\"a\" to 1, \"b\" to 2, \"c\" to 3)\n\
         \x20   var keys = \"\"\n\
         \x20   for (k in m.keys) keys += k\n\
         \x20   if (keys != \"abc\") return \"fail keys \" + keys\n\
         \x20   var values = \"\"\n\
         \x20   for (v in m.values) values += v\n\
         \x20   if (values != \"123\") return \"fail values \" + values\n\
         \x20   var entries = \"\"\n\
         \x20   for (e in m.entries) entries += \"\" + e.key + e.value\n\
         \x20   if (entries != \"a1b2c3\") return \"fail entries \" + entries\n\
         \x20   // `for ((k, v) in m)` walks the ENTRIES, which is what Kotlin's own\n\
         \x20   // `Map.iterator()` answers, and destructures each.\n\
         \x20   var both = \"\"\n\
         \x20   for ((k, v) in m) both += \"\" + k + v\n\
         \x20   if (both != \"a1b2c3\") return \"fail destructured \" + both\n\
         \x20   if (\"\" + m != \"{a=1, b=2, c=3}\") return \"fail toString \" + m\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "MapWalks");
    expect_native_box(source, "MapWalks", "OK");
}

/// A set keeps each element once and remembers the order it first saw them in.
#[test]
fn a_set_keeps_each_element_once_in_the_order_it_first_saw_them() {
    let source = "fun box(): String {\n\
         \x20   val s = setOf(1, 2, 2, 3)\n\
         \x20   if (s.size != 3) return \"fail size\"\n\
         \x20   if (2 !in s || 9 in s) return \"fail contains\"\n\
         \x20   if (s.isEmpty()) return \"fail isEmpty\"\n\
         \x20   var walked = \"\"\n\
         \x20   for (x in s) walked += x\n\
         \x20   if (walked != \"123\") return \"fail order \" + walked\n\
         \x20   if (setOf(7).size != 1) return \"fail one element\"\n\
         \x20   if (!emptySet<Int>().isEmpty()) return \"fail empty\"\n\
         \x20   val m = mutableSetOf<Int>()\n\
         \x20   if (!m.add(1)) return \"fail added\"\n\
         \x20   if (m.add(1)) return \"fail added twice\"\n\
         \x20   if (m.size != 1) return \"fail dedupe\"\n\
         \x20   if (!m.remove(1) || m.remove(1)) return \"fail remove\"\n\
         \x20   val h = HashSet<Int>()\n\
         \x20   h.add(5)\n\
         \x20   if (h.size != 1 || 5 !in h) return \"fail HashSet\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SetElements");
    expect_native_box(source, "SetElements", "OK");
}

/// The three `kotlin.Any` members. Neither a map nor a set says anything about ORDER in them.
#[test]
fn two_maps_are_equal_when_they_hold_the_same_entries_in_any_order() {
    let source = "fun box(): String {\n\
         \x20   if (mapOf(\"a\" to 1, \"b\" to 2) != mapOf(\"b\" to 2, \"a\" to 1))\n\
         \x20       return \"fail order\"\n\
         \x20   if (mapOf(\"a\" to 1) == mapOf(\"a\" to 2)) return \"fail value\"\n\
         \x20   if (mapOf(\"a\" to 1) == mapOf(\"a\" to 1, \"b\" to 2)) return \"fail size\"\n\
         \x20   if (mapOf(\"a\" to 1).hashCode() != mapOf(\"a\" to 1).hashCode()) return \"fail hash\"\n\
         \x20   if (setOf(1, 2) != setOf(2, 1)) return \"fail set order\"\n\
         \x20   if (setOf(1, 2).hashCode() != setOf(2, 1).hashCode()) return \"fail set hash\"\n\
         \x20   if (\"\" + setOf(1, 2) != \"[1, 2]\") return \"fail set toString\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "MapIdentity");
    expect_native_box(source, "MapIdentity", "OK");
}

/// A key is found by `equals`, not by identity — two strings with the same text are one key.
#[test]
fn a_key_is_found_by_equality_and_not_by_identity() {
    let source = "fun box(): String {\n\
         \x20   val m = mutableMapOf<String, Int>()\n\
         \x20   m[\"a\" + \"b\"] = 1\n\
         \x20   if (m[\"ab\"] != 1) return \"fail text\"\n\
         \x20   m[\"ab\"] = 2\n\
         \x20   if (m.size != 1) return \"fail one key\"\n\
         \x20   val n = mutableMapOf<Int, String>()\n\
         \x20   n[1 + 1] = \"two\"\n\
         \x20   if (n[2] != \"two\") return \"fail number\"\n\
         \x20   // A null VALUE is a value, and is not an absent key.\n\
         \x20   val z = mutableMapOf<String, Int?>()\n\
         \x20   z[\"k\"] = null\n\
         \x20   if (!z.containsKey(\"k\")) return \"fail null value\"\n\
         \x20   if (z.size != 1) return \"fail null size\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "MapKeys");
    expect_native_box(source, "MapKeys", "OK");
}
