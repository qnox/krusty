//! Class-body properties (`class C { val x = … }`), plain (non-property) constructor parameters,
//! and `init { }` blocks — initialized in the primary constructor, accessible from member methods.
//! Plus open-property virtual dispatch (an `open val` read inside the class calls the getter).

use super::common;

fn run_box(_name: &str, src: &str) {
    common::expect_box_ok_with_stdlib(src, "B");
}

#[test]
fn body_properties_and_init_block() {
    run_box("init", "class Counter(start: Int) {\n  val initial: Int = start\n  var count: Int = 0\n  init { count = start * 2 }\n  fun total(): Int = initial + count\n}\nfun box(): String {\n  val c = Counter(5)\n  if (c.initial != 5) return \"f1\"\n  if (c.count != 10) return \"f2\"\n  if (c.total() != 15) return \"f3\"\n  return \"OK\"\n}\n");
}

#[test]
fn open_property_virtual_dispatch() {
    // An `open val` read inside the base class must dispatch to the override.
    run_box("openprop", "open class Base { open val kind: String = \"base\"\n  fun k(): String = kind\n}\nclass Sub : Base() { override val kind: String = \"sub\" }\nfun box(): String = if (Sub().k() == \"sub\") \"OK\" else \"fail\"\n");
}

#[test]
fn open_property_virtual_dispatch_through_a_grandparent() {
    // Three levels: the read in `Base` must reach the MOST DERIVED override, and the intermediate
    // class's own override must not shadow it. This shape is what the old whole-file
    // `gate:base-reads-override-internally` bail refused (a base that itself has a base).
    run_box(
        "openpropdeep",
        "open class Base { open val kind: String = \"base\"\n  fun k(): String = kind\n}\nopen class Mid : Base() { override val kind: String = \"mid\" }\nclass Leaf : Mid() { override val kind: String = \"leaf\" }\nfun box(): String {\n  if (Leaf().k() != \"leaf\") return \"f1\"\n  if (Mid().k() != \"mid\") return \"f2\"\n  if (Base().k() != \"base\") return \"f3\"\n  return \"OK\"\n}\n",
    );
}

/// The two box-corpus cases that used to be pinned to `gate:base-reads-override-internally` in
/// `deep_class_bail_reason_e2e.rs`: a base method reading two overridden `val`s, and an override that
/// also substitutes a generic supertype's property. Both must now RUN, not merely stop bailing.
#[test]
fn base_reads_overridden_property_corpus_cases_run() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "properties/kt1168.kt",
        "bridges/substitutionInSuperClass/property.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must box()=OK"
        );
    }
}
