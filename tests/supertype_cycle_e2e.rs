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
