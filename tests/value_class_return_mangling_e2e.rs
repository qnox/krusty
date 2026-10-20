//! A member's own value-class result names it, whatever it overrides.
//!
//! kotlinc names a member of a class (not a file facade) whose own result is a value class by its
//! own signature: `InlineClassAbi.hashSuffix` hashes the value parameters and the member's own
//! return type, once, into one suffix. An override of a generic member (`override val key: Z` for
//! `Map.Entry<K, V>.key`) or of a member typed differently (`Z` for `Z?`) is no exception: its
//! getter is `getKey-<hash>()I`, and the supertype's own spelling is a bridge that calls it.
//!
//! krusty kept such an override getter under its plain name, wrote no bridge for a getter typed
//! `Z` over `Z?` (both share a descriptor before value classes are erased, so an interface call
//! failed with `AbstractMethodError`), and named a value class's interface
//! entry twice, appending a second hash over the entry's physical result to the member's name
//! (`get-ayyZXx8--zXLvJo` for `get-ayyZXx8`).

use super::common;

/// Each listed class is written byte for byte as kotlinc writes it.
fn assert_classes_match_kotlinc(file: &str, source: &str, classes: &[&str]) {
    let built = common::classes_against_kotlinc_module(&[(file, source)]);
    let mismatches = classes
        .iter()
        .filter_map(|&class| {
            let reference = built
                .reference
                .get(class)
                .unwrap_or_else(|| panic!("kotlinc writes {class}"));
            let ours = built
                .krusty
                .get(class)
                .unwrap_or_else(|| panic!("krusty writes {class}"));
            (reference != ours).then(|| common::exact_class_difference(class, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

const OVERRIDE_GETTERS: &str = "@JvmInline value class Z(val x: Int)\n\
@JvmInline value class S(val x: String)\n\
interface Generic<T> { val p: T }\n\
interface NullableGeneric<T> { val p: T? }\n\
interface Nullable { val p: Z? }\n\
interface Top { val p: Any }\n\
open class Base<T>(open val p: T)\n\
class OverGeneric : Generic<Z> { override val p: Z get() = Z(1) }\n\
class OverNullableGeneric : NullableGeneric<Z> { override val p: Z get() = Z(2) }\n\
class OverNullable : Nullable { override val p: Z get() = Z(3) }\n\
class StoredOverNullable : Nullable { override val p: Z = Z(0) }\n\
class OverTop : Top { override val p: Z get() = Z(4) }\n\
class NullableOverGeneric : Generic<Z?> { override val p: Z? get() = null }\n\
class ReferenceOverGeneric : Generic<S> { override val p: S get() = S(\"5\") }\n\
class OverClass : Base<Z>(Z(0)) { override val p: Z get() = Z(6) }\n\
class Entry(val k: Int, val v: Int) : Map.Entry<Z, Z> {\n\
\x20   override val key: Z get() = Z(k)\n\
\x20   override val value: Z get() = Z(v)\n\
}\n\
abstract class KeyedEntry(val k: Int) : Map.Entry<Z, Z> { override val key: Z get() = Z(k) }\n\
class ValuedEntry(k: Int) : KeyedEntry(k) { override val value: Z get() = Z(k + 1) }\n\
fun box(): String {\n\
\x20   val generic: Generic<Z> = OverGeneric()\n\
\x20   val nullableGeneric: NullableGeneric<Z> = OverNullableGeneric()\n\
\x20   val nullable: Nullable = OverNullable()\n\
\x20   val top: Top = OverTop()\n\
\x20   val entry: Map.Entry<Z, Z> = Entry(7, 8)\n\
\x20   val result = \"\" + generic.p.x + nullableGeneric.p?.x + nullable.p?.x + (top.p as Z).x +\n\
\x20       OverGeneric().p.x + NullableOverGeneric().p + ReferenceOverGeneric().p.x +\n\
\x20       (OverClass() as Base<Z>).p.x + OverClass().p.x + entry.key.x + entry.value.x + Entry(9, 0).key.x +\n\
\x20       (StoredOverNullable() as Nullable).p?.x\n\
\x20   val valued: Map.Entry<Z, Z> = ValuedEntry(1)\n\
\x20   val entries = \"\" + valued.key.x + valued.value.x + ValuedEntry(3).value.x\n\
\x20   return if (result == \"12341null5667890\" && entries == \"124\") \"OK\" else result\n\
}\n";

// `Entry` and `KeyedEntry` run below but are left out of the byte comparison: as the first classes
// implementing the read-only `Map.Entry` they also take kotlinc's `setValue` collection stubs,
// which krusty does not write yet. `ValuedEntry` inherits those stubs and overrides `value` alone.
#[test]
fn an_override_getter_is_named_by_its_own_value_class_result() {
    assert_classes_match_kotlinc(
        "OverrideGetters.kt",
        OVERRIDE_GETTERS,
        &[
            "OverGeneric",
            "OverNullableGeneric",
            "OverNullable",
            "StoredOverNullable",
            "OverTop",
            "NullableOverGeneric",
            "ReferenceOverGeneric",
            "OverClass",
            "ValuedEntry",
        ],
    );
}

#[test]
fn an_override_getter_answers_through_the_supertype_bridge() {
    common::expect_box_ok_with_stdlib(OVERRIDE_GETTERS, "OverrideGetters");
}

const VALUE_CLASS_ENTRIES: &str = "@JvmInline value class Z(val x: Int)\n\
interface Generic<T> { fun get(k: T): T?; fun none(): T? }\n\
interface Exact { fun get(k: Z): Z?; fun none(): Z? }\n\
@JvmInline value class OverGeneric(val s: Int) : Generic<Z> {\n\
\x20   override fun get(k: Z): Z? = Z(k.x + s)\n\
\x20   override fun none(): Z? = null\n\
}\n\
@JvmInline value class OverExact(val s: Int) : Exact {\n\
\x20   override fun get(k: Z): Z? = Z(k.x * s)\n\
\x20   override fun none(): Z? = null\n\
}\n\
fun box(): String {\n\
\x20   val generic: Generic<Z> = OverGeneric(1)\n\
\x20   val exact: Exact = OverExact(3)\n\
\x20   val result = \"\" + generic.get(Z(1))?.x + generic.none() + exact.get(Z(2))?.x + exact.none() +\n\
\x20       OverGeneric(2).get(Z(2))?.x + OverExact(1).none()\n\
\x20   return if (result == \"2null6null4null\") \"OK\" else result\n\
}\n";

#[test]
fn a_value_class_interface_entry_keeps_the_members_single_hash() {
    assert_classes_match_kotlinc(
        "ValueClassEntries.kt",
        VALUE_CLASS_ENTRIES,
        &["OverGeneric", "OverExact"],
    );
}

#[test]
fn a_value_class_interface_entry_answers_interface_calls() {
    common::expect_box_ok_with_stdlib(VALUE_CLASS_ENTRIES, "ValueClassEntries");
}
