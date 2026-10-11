//! The members Kotlin declares on `Collection` and `MutableCollection`, asked of a receiver typed
//! by one of those two rather than by a narrower list or set.
//!
//! Behind that static type may be either list shape, a set, or one of a map's `keys`, `values` and
//! `entries` views, and nothing at the call site says which. The runtime's entry points ask the
//! receiver's descriptor and hand over to that shape's own answer, the way iteration already does.
//! A narrower static type keeps its shape's direct entry point; `containsAll` and `addAll` are
//! answered there too.
//!
//! Every program also runs on the JVM, which is what the expected `OK` is checked against.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

fn expect_ok(source: &str, stem: &str) {
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

#[test]
fn a_collection_answers_its_size_whatever_stands_behind_it() {
    expect_ok(
        "fun count(values: Collection<String>): Int = values.size\n\
         fun <T> sizeOf(values: Collection<T>): Int = values.size\n\
         fun box(): String {\n\
         \x20   if (count(listOf(\"a\", \"b\", \"c\")) != 3) return \"fail list\"\n\
         \x20   if (sizeOf(listOf<Int>()) != 0) return \"fail empty\"\n\
         \x20   val growing = mutableListOf(\"a\")\n\
         \x20   growing.add(\"b\")\n\
         \x20   if (count(growing) != 2) return \"fail growing\"\n\
         \x20   if (count(setOf(\"a\", \"b\", \"a\")) != 2) return \"fail set\"\n\
         \x20   if (count(emptySet()) != 0) return \"fail empty set\"\n\
         \x20   val map = mapOf(1 to \"x\", 2 to \"y\", 3 to \"z\")\n\
         \x20   if (sizeOf(map.keys) != 3) return \"fail keys\"\n\
         \x20   if (sizeOf(map.values) != 3) return \"fail values\"\n\
         \x20   if (sizeOf(map.entries) != 3) return \"fail entries\"\n\
         \x20   val nothing: Collection<String>? = null\n\
         \x20   if ((nothing?.size ?: -1) != -1) return \"fail null\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "CollectionSize",
    );
}

#[test]
fn a_collection_answers_emptiness_and_membership() {
    expect_ok(
        "fun describe(values: Collection<String>): String {\n\
         \x20   var out = values.size.toString()\n\
         \x20   out += if (values.isEmpty()) \"E\" else \"N\"\n\
         \x20   out += if (values.contains(\"b\")) \"b\" else \"-\"\n\
         \x20   out += if (\"z\" in values) \"z\" else \"-\"\n\
         \x20   for (v in values) out += v\n\
         \x20   return out\n\
         }\n\
         fun box(): String {\n\
         \x20   val list = describe(listOf(\"a\", \"b\"))\n\
         \x20   if (list != \"2Nb-ab\") return \"fail list: \" + list\n\
         \x20   val set = describe(setOf(\"b\", \"c\", \"z\"))\n\
         \x20   if (set != \"3Nbzbcz\") return \"fail set: \" + set\n\
         \x20   val values = describe(mapOf(1 to \"z\").values)\n\
         \x20   if (values != \"1N-zz\") return \"fail values: \" + values\n\
         \x20   val empty = describe(emptyList())\n\
         \x20   if (empty != \"0E--\") return \"fail empty: \" + empty\n\
         \x20   return \"OK\"\n\
         }\n",
        "CollectionMembership",
    );
}

#[test]
fn contains_all_asks_each_element_of_the_argument() {
    expect_ok(
        "fun all(c: Collection<String>, d: Collection<String>) = c.containsAll(d)\n\
         fun box(): String {\n\
         \x20   if (!all(listOf(\"a\", \"b\"), setOf(\"a\"))) return \"fail list\"\n\
         \x20   if (all(setOf(\"a\"), listOf(\"a\", \"z\"))) return \"fail set\"\n\
         \x20   if (!all(setOf(\"a\"), emptyList())) return \"fail empty argument\"\n\
         \x20   if (!all(mapOf(\"k\" to 1).keys, listOf(\"k\"))) return \"fail keys\"\n\
         \x20   if (!listOf(1, 2, 3).containsAll(listOf(3, 1))) return \"fail typed list\"\n\
         \x20   if (setOf(1, 2).containsAll(listOf(1, 4))) return \"fail typed set\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "CollectionContainsAll",
    );
}

#[test]
fn a_mutable_collection_changes_whatever_stands_behind_it() {
    expect_ok(
        "fun edit(c: MutableCollection<String>): String {\n\
         \x20   val added = c.add(\"x\")\n\
         \x20   val removed = c.remove(\"a\")\n\
         \x20   val missing = c.remove(\"nope\")\n\
         \x20   return \"$added$removed$missing${c.size}\"\n\
         }\n\
         fun box(): String {\n\
         \x20   val list = mutableListOf(\"a\", \"b\")\n\
         \x20   val l = edit(list)\n\
         \x20   if (l != \"truetruefalse2\" || list.toString() != \"[b, x]\") return \"fail list: $l $list\"\n\
         \x20   val set = mutableSetOf(\"a\", \"x\")\n\
         \x20   val s = edit(set)\n\
         \x20   if (s != \"falsetruefalse1\" || set.toString() != \"[x]\") return \"fail set: $s $set\"\n\
         \x20   val map = mutableMapOf(1 to \"a\", 2 to \"b\")\n\
         \x20   val values: MutableCollection<String> = map.values\n\
         \x20   if (!values.remove(\"a\") || map.toString() != \"{2=b}\") return \"fail view remove: $map\"\n\
         \x20   values.clear()\n\
         \x20   if (map.size != 0) return \"fail view clear\"\n\
         \x20   val cleared: MutableCollection<String> = list\n\
         \x20   cleared.clear()\n\
         \x20   if (list.size != 0) return \"fail list clear\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MutableCollectionMembers",
    );
}

#[test]
fn add_all_answers_whether_the_receiver_changed() {
    expect_ok(
        "fun grow(c: MutableCollection<Int>, more: Collection<Int>) = c.addAll(more)\n\
         fun box(): String {\n\
         \x20   val list = mutableListOf(1)\n\
         \x20   if (!grow(list, listOf(1, 2))) return \"fail list\"\n\
         \x20   if (grow(list, emptyList())) return \"fail list empty\"\n\
         \x20   if (list.toString() != \"[1, 1, 2]\") return \"fail list content: $list\"\n\
         \x20   if (!list.addAll(list) || list.size != 6) return \"fail list itself: $list\"\n\
         \x20   val set = mutableSetOf(1)\n\
         \x20   if (!grow(set, listOf(1, 2))) return \"fail set\"\n\
         \x20   if (grow(set, listOf(2))) return \"fail set again\"\n\
         \x20   if (!set.addAll(setOf(3)) || set.toString() != \"[1, 2, 3]\") return \"fail typed set: $set\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MutableCollectionAddAll",
    );
}

/// A dependency subtype of `Collection` that declares its own `size` and mutators: the selected
/// declarations are `ArrayDeque`'s, not the interface's, so none of them enters the runtime's
/// collection entry points — the runtime does not make an `ArrayDeque` and would read a header
/// that is not there. Each declines at its own declaration.
#[test]
fn a_dependency_subtypes_own_size_is_not_the_collections() {
    expect_native_decline(
        "fun count(values: ArrayDeque<String>): Int = values.size\n\
         fun box(): String = \"OK\"\n",
        "DependencySubtypeSize",
        "kotlin/collections/ArrayDeque.size",
    );
}

#[test]
fn a_dependency_subtypes_own_mutators_are_not_the_collections() {
    expect_native_decline(
        "fun wipe(values: ArrayDeque<String>) = values.clear()\n\
         fun box(): String = \"OK\"\n",
        "DependencySubtypeClear",
        "ArrayDeque.clear",
    );
    expect_native_decline(
        "fun grow(values: ArrayDeque<String>) = values.add(\"x\")\n\
         fun box(): String = \"OK\"\n",
        "DependencySubtypeAdd",
        "ArrayDeque.add",
    );
}

#[test]
fn dependency_map_and_set_subtypes_do_not_borrow_runtime_properties() {
    expect_native_decline(
        "fun keys(values: AbstractMap<String, String>) = values.keys\n\
         fun box(): String = \"OK\"\n",
        "DependencySubtypeMapKeys",
        "kotlin/collections/AbstractMap.keys",
    );
    expect_native_decline(
        "fun count(values: AbstractSet<String>): Int = values.size\n\
         fun box(): String = \"OK\"\n",
        "DependencySubtypeSetSize",
        "kotlin/collections/AbstractCollection.size",
    );
}
