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

/// The other half of the rule: a WRITE, and a property declared in the PRIMARY CONSTRUCTOR rather
/// than the class body. Both spellings of the write (bare `v = …` and qualified `this.v = …`) and the
/// read-modify-write form must all go through `setV`, or a base member stores into the base's own
/// field and the subclass override never sees it. Values verified against kotlinc.
#[test]
fn open_property_writes_and_constructor_declarations_dispatch_virtually() {
    run_box(
        "openpropwrite",
        "open class Ctor(open var v: Int) {\n\
  fun bare() { v = 5 }\n\
  fun qualified() { this.v = 6 }\n\
  fun incremented() { v++ }\n\
  fun read(): Int = v\n\
}\n\
class CtorSub : Ctor(0) { override var v: Int = 100 }\n\
open class Body { open var w: Int = 0\n\
  fun bare() { w = 5 }\n\
  fun read(): Int = w\n\
}\n\
class BodySub : Body() { override var w: Int = 100 }\n\
open class CtorVal(open val item: String) { fun show(): String = item }\n\
class CtorValSub : CtorVal(\"a\") { override val item: String = \"A\" }\n\
fun box(): String {\n\
  val c = CtorSub()\n\
  c.bare()\n\
  if (c.read() != 5 || c.v != 5) return \"f1\"\n\
  c.qualified()\n\
  if (c.read() != 6 || c.v != 6) return \"f2\"\n\
  c.incremented()\n\
  if (c.read() != 7 || c.v != 7) return \"f3\"\n\
  val b = BodySub()\n\
  b.bare()\n\
  if (b.read() != 5 || b.w != 5) return \"f4\"\n\
  if (CtorValSub().show() != \"A\") return \"f5\"\n\
  return \"OK\"\n\
}\n",
    );
}

/// A base's own `init { }` assignment to an `open var` also goes through the setter (kotlinc does the
/// same); only the property INITIALIZER stays a `putfield`. Pinned because routing the initializer
/// through a subclass setter would store into a field that does not exist yet.
#[test]
fn open_var_init_block_writes_through_the_setter() {
    run_box(
        "openpropinit",
        "open class Base { open var n: Int = 1\n  init { n = 7 }\n  fun read(): Int = n\n}\nclass Sub : Base() { override var n: Int = 100 }\nfun box(): String {\n  if (Base().read() != 7) return \"f1\"\n  if (Sub().read() != 100) return \"f2\"\n  if (Sub().n != 100) return \"f3\"\n  return \"OK\"\n}\n",
    );
}

/// Box-corpus pins for the open-property rule, run directly so a break is attributable here rather
/// than only in the full conformance sweep:
///
/// * the two cases that used to sit behind `gate:base-reads-override-internally` in
///   `deep_class_bail_reason_e2e.rs` — a base method reading two overridden `val`s, and an override
///   that also substitutes a generic supertype's property. Both must now RUN, not merely stop bailing.
/// * a deferred `open val` initialization (`open val c: B` assigned in `init { }`, legal under
///   `-ProhibitOpenValDeferredInitialization`). A `val` declares no SETTER, so this write must stay a
///   `putfield` — routing it through `set<Name>` like an open `var` is a `NoSuchMethodError`. The case
///   carries the LANGUAGE directive, which is why it is pinned from the corpus and not hand-written:
///   kotlinc rejects the same source by default ("property must be initialized, be final, or be
///   abstract").
#[test]
fn open_property_corpus_cases_run() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "properties/kt1168.kt",
        "bridges/substitutionInSuperClass/property.kt",
        "operatorConventions/augmentedAssignmentInInitializer.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must box()=OK"
        );
    }
}
