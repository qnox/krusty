//! A local class whose SUPERCLASS is a local class with captures.
//!
//! A capturing local class takes its captures as synthetic PREFIX parameters of its constructor,
//! ahead of the ones the source wrote. A subclass's `super(…)` spells only the written ones — the
//! prefix is not in the source and there is no expression there to resolve — so two things had to
//! be supplied for such a subclass to be constructible at all: it has to CARRY the superclass's
//! captures (selection records them as its own, read from the resolved supertype rather than from a
//! call, because a supertype constructor call is not an expression), and its `super(…)` has to PASS
//! them on ahead of the written arguments.
//!
//! Without either half the call was one value short per capture, and the emitted class was rejected
//! with `VerifyError: Bad type on operand stack` — `this` sat where the capture belonged. kotlinc
//! compiles every program here, so every expectation is taken by running it under kotlinc.
//!
//! An ANONYMOUS OBJECT extending a capturing local class is the same shape one level over and is
//! NOT covered: its captures are discovered on a different path, from the construction site's
//! candidate list rather than from a declaration's resolved supertype, and that path has no
//! superclass edge to read yet.

use super::common;

/// Run `body` under krusty AND under the reference compiler, and require the SAME output.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    let reference = common::kotlinc_box_result(body);
    assert_eq!(reference, "OK", "{stem}: unexpected kotlinc result");
    assert_eq!(krusty, reference, "{stem}: krusty and kotlinc disagree");
}

/// The subclass captures NOTHING of its own: everything it carries is the superclass's.
#[test]
fn a_local_subclass_carries_its_superclasss_capture() {
    agrees_with_kotlinc(
        "LocalSubclassCarriesCapture",
        "fun foo(s: String): String {\n\
         \x20   open class Local {\n\
         \x20       fun f() = s\n\
         \x20   }\n\
         \x20   open class Derived : Local() {\n\
         \x20       fun g() = f()\n\
         \x20   }\n\
         \x20   return Derived().g()\n\
         }\n\
         fun box(): String = foo(\"OK\")\n",
    );
}

/// The superclass takes a WRITTEN argument beside its capture, so the prefix goes ahead of it.
#[test]
fn a_captured_prefix_precedes_the_written_super_arguments() {
    agrees_with_kotlinc(
        "CapturedPrefixPrecedesWritten",
        "fun box(): String {\n\
         \x20   val result = \"OK\"\n\
         \x20   open class Local(val ok: Boolean) {\n\
         \x20       fun result() = if (ok) result else \"Fail\"\n\
         \x20   }\n\
         \x20   class Derived : Local(true)\n\
         \x20   return Derived().result()\n\
         }\n",
    );
}

/// Both capture, and they capture DIFFERENT values: the subclass carries its own and the
/// superclass's, and passes on only the superclass's.
#[test]
fn a_subclass_with_its_own_capture_still_passes_the_superclasss() {
    agrees_with_kotlinc(
        "SubclassWithItsOwnCapture",
        "fun box(): String {\n\
         \x20   val left = \"O\"\n\
         \x20   val right = \"K\"\n\
         \x20   open class Local {\n\
         \x20       fun first() = left\n\
         \x20   }\n\
         \x20   class Derived : Local() {\n\
         \x20       fun both() = first() + right\n\
         \x20   }\n\
         \x20   return Derived().both()\n\
         }\n",
    );
}

/// The same value captured by both: one capture, not two, and the subclass passes it on.
#[test]
fn one_value_captured_by_both_is_carried_once() {
    agrees_with_kotlinc(
        "OneValueCapturedByBoth",
        "fun box(): String {\n\
         \x20   val shared = \"OK\"\n\
         \x20   open class Local {\n\
         \x20       fun base() = shared\n\
         \x20   }\n\
         \x20   class Derived : Local() {\n\
         \x20       fun mine() = shared\n\
         \x20   }\n\
         \x20   val derived = Derived()\n\
         \x20   return if (derived.base() == derived.mine()) derived.mine() else \"Fail\"\n\
         }\n",
    );
}

/// THREE levels, each link passing the capture to the next. The member is reached through the
/// middle class, because a call naming a member two levels up a LOCAL hierarchy does not resolve
/// today — a separate, pre-existing gap that has nothing to do with captures.
#[test]
fn a_capture_travels_a_three_level_local_hierarchy() {
    agrees_with_kotlinc(
        "CaptureTravelsThreeLevels",
        "fun box(): String {\n\
         \x20   val result = \"OK\"\n\
         \x20   open class First {\n\
         \x20       fun value() = result\n\
         \x20   }\n\
         \x20   open class Second : First() {\n\
         \x20       fun middle() = value()\n\
         \x20   }\n\
         \x20   class Third : Second() {\n\
         \x20       fun last() = middle()\n\
         \x20   }\n\
         \x20   return Third().last()\n\
         }\n",
    );
}

/// A MUTABLE capture is a shared cell, and the subclass must pass on the cell rather than a copy:
/// a write through the enclosing function is visible to the superclass's read.
#[test]
fn a_mutable_capture_stays_one_cell_across_the_hierarchy() {
    agrees_with_kotlinc(
        "MutableCaptureAcrossHierarchy",
        "fun box(): String {\n\
         \x20   var state = \"Fail\"\n\
         \x20   open class Local {\n\
         \x20       fun read() = state\n\
         \x20   }\n\
         \x20   class Derived : Local()\n\
         \x20   val derived = Derived()\n\
         \x20   state = \"OK\"\n\
         \x20   return derived.read()\n\
         }\n",
    );
}
