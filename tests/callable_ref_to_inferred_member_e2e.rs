//! A callable reference to a member whose OWN type is still being determined.
//!
//! `class C { val bar = 42 }` / `val b = C::bar` types the reference as
//! `KProperty1<C, <not determined>>`: the reference read `bar`'s record while `bar` was itself being
//! resolved. The member READ path already re-asks the engine in that situation; a reference did not,
//! so the marker travelled out as the declaration's type and aborted metadata emission.
//!
//! The two spellings name their owner differently: an UNBOUND `C::bar` spells a classifier, which is
//! not a value, so the owner comes from the reference's own type arguments; a BOUND `C()::bar` spells
//! a value, and its classifier carries only the member's type, so the owner is the receiver's.
//! Annotating the member always worked, which is what made this specific to inferred ones.
use super::common;

#[test]
fn an_unbound_property_reference_to_an_inferred_member() {
    const SRC: &str = "class C { val bar = 42 }\n\
        val b = C::bar\n\
        fun box(): String = if (b.get(C()) == 42) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "UnboundRefInferredMember");
}

#[test]
fn a_bound_property_reference_to_an_inferred_member() {
    const SRC: &str = "class C { val bar = 42 }\n\
        val c = C()::bar\n\
        fun box(): String = if (c.get() == 42) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "BoundRefInferredMember");
}

#[test]
fn a_function_reference_to_an_inferred_member_is_unaffected() {
    // The control: a FUNCTION reference records its type beside the expression table, and its return
    // was never the marker here.
    const SRC: &str = "class C { fun foo() = 42 }\n\
        val f = C::foo\n\
        fun box(): String = if (f(C()) == 42) \"OK\" else \"F\"\n";
    common::expect_box_ok_with_stdlib(SRC, "FunctionRefInferredMember");
}
