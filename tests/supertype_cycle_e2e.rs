//! A cyclic supertype hierarchy is ill-formed source, and must produce a DIAGNOSTIC rather than
//! exhaust the stack.
//!
//! krusty's hierarchy walkers are recursive. A class whose supertype chain reaches itself sent one
//! of them into unbounded recursion until the process died on a SIGBUS with no diagnostic at all —
//! found on 13 modules of intellij-community by the parity scan, where a same-named import that does
//! not resolve leaves a supertype bound to the declaration itself (`object LZ4Compressor :
//! LZ4Compressor()` in `platform/util-ex/.../lz4.kt`).
//!
//! kotlinc 2.4.10 answers these with "cycle in supertypes and/or containing declarations detected."

use super::common;

fn diagnostics(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn reports_a_cycle(src: &str) -> bool {
    diagnostics(src)
        .iter()
        .any(|d| d.contains("cycle in supertypes"))
}

/// The shape that crashed: an object whose supertype is its own name.
#[test]
fn an_object_extending_itself_is_reported_not_fatal() {
    assert!(
        reports_a_cycle("object Foo : Foo() {\n    fun f(): Int = 0\n}\n"),
        "{:?}",
        diagnostics("object Foo : Foo() {\n    fun f(): Int = 0\n}\n")
    );
}

/// A class extending itself, the same cycle through a different declaration kind.
#[test]
fn a_class_extending_itself_is_reported() {
    assert!(reports_a_cycle("open class A : A()\n"));
}

/// An INDIRECT cycle: neither declaration names itself, so a guard that only compares against the
/// starting class would miss it and recurse forever.
#[test]
fn a_two_step_cycle_is_reported() {
    assert!(reports_a_cycle("open class A : B()\nopen class B : A()\n"));
}

/// A cycle through an interface, which travels the `interfaces` edge rather than the superclass one.
#[test]
fn a_cycle_through_an_interface_is_reported() {
    assert!(reports_a_cycle("interface I : J\ninterface J : I\n"));
}

/// An interface that merely sits ALONGSIDE a cyclic superclass is innocent. Cutting it too made its
/// members vanish, and krusty then reported `'m' overrides nothing` on a method kotlinc accepts.
#[test]
fn an_innocent_interface_survives_the_cut() {
    let source = "interface Marker { fun m(): Int }\n                  open class Cyc : Cyc(), Marker { override fun m(): Int = 1 }\n";
    let reported = diagnostics(source);
    assert!(
        reported.iter().any(|d| d.contains("cycle in supertypes")),
        "the cycle is still reported: {reported:?}"
    );
    assert!(
        !reported.iter().any(|d| d.contains("overrides nothing")),
        "the interface's members must survive, so the override still has a base: {reported:?}"
    );
}

/// The cycle must be CUT, not merely reported: a diagnostic alone leaves the cyclic edge in place
/// for every later walker, and the recursion that follows takes down the whole test binary rather
/// than failing one case. Compiling a subclass of a cyclic class exercises those walkers.
#[test]
fn the_cyclic_edge_is_cut_so_later_walks_terminate() {
    let reported =
        diagnostics("open class Cyc : Cyc()\nclass Sub : Cyc()\nfun use(s: Sub): Cyc = s\n");
    assert!(
        reported.iter().any(|d| d.contains("cycle in supertypes")),
        "{reported:?}"
    );
}

/// Diagnostics are emitted in SOURCE order. The pass collects from a hash map, whose iteration order
/// varies per run; every other krusty path — and kotlinc — reports in source order.
#[test]
fn cycles_are_reported_in_source_order() {
    let source = "open class A1 : A2()\nopen class A2 : A3()\nopen class A3 : A1()\n";
    let first = diagnostics(source);
    assert_eq!(first.len(), 3, "one per class in the cycle: {first:?}");
    for _ in 0..4 {
        assert_eq!(
            diagnostics(source),
            first,
            "the order must not vary between runs"
        );
    }
}

/// A cycle that does not pass through the scanned class: `A` reaches `B -> C -> B` but is not itself
/// in the cycle. kotlinc reports `B` and `C` and not `A`; so does krusty.
#[test]
fn only_the_classes_actually_in_the_cycle_are_reported() {
    let reported = diagnostics("open class A : B()\nopen class B : C()\nopen class C : B()\n");
    assert_eq!(
        reported.len(),
        2,
        "B and C are in the cycle, A only reaches it: {reported:?}"
    );
}

/// The guard must not fire on ordinary hierarchies — including a diamond, where the same supertype
/// is reached twice by different paths and a naive "already visited" test would call it a cycle.
#[test]
fn an_ordinary_hierarchy_is_not_reported() {
    for source in [
        "open class Base\nclass Derived : Base()\n",
        "interface Top\ninterface Left : Top\ninterface Right : Top\nclass Both : Left, Right\n",
        "open class A\nopen class B : A()\nclass C : B()\n",
    ] {
        assert!(
            !reports_a_cycle(source),
            "a well-formed hierarchy must compile: {source:?} -> {:?}",
            diagnostics(source)
        );
    }
}

/// An unresolved type-parameter bound is a FRONTEND error. It must never reach metadata emission,
/// panic there, or silently produce a Kotlin class without authoritative Kotlin metadata.
#[test]
fn an_unresolved_type_parameter_bound_stops_before_metadata_emission() {
    let source = "abstract class C<P>(val p: P) where P : DefinitelyAbsentBoundA, P : DefinitelyAbsentBoundB\n";
    let reported = diagnostics(source);
    assert_eq!(
        reported,
        [
            "unresolved reference 'DefinitelyAbsentBoundA'.",
            "unresolved reference 'DefinitelyAbsentBoundB'.",
        ]
    );
    let classes = common::compile_in_process(source, "P", &[], None);
    assert!(
        classes.is_none(),
        "a frontend error must stop emission instead of dropping metadata"
    );
}
