//! An IMPLICIT-receiver call to a same-class member that OMITS a CONSTANT-default argument
//! (`resolveRoles(id)` on `resolveRoles(id, filter = null)` — the shape mission-core's RbacService uses).
//! Two coupled gaps:
//!
//!  1. `lower_this_member_call` only handled exact arity / vararg — an omitted default made the bare-name
//!     member call fall through every branch and the file bailed ("construct not yet supported"). It now
//!     fills the omitted CONSTANT defaults at the call site and invokes the plain method (semantically
//!     identical to kotlinc's `$default` for a constant default), and the sibling-member branch fires on
//!     `resolve_method` (not only the checker's `resolved_member`, which it omits for a defaulted call).
//!  2. A FORWARD reference (the caller is defined BEFORE the defaulted method in the class) saw only the
//!     pass-1 "has defaults" marker (an empty vec), since pass-2 default lowering runs per-method-body in
//!     source order. Pass 1 now lowers CONSTANT defaults immediately, so the forward reference can fill.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn implicit_receiver_omits_constant_default() {
    const SRC: &str = "class C {\n\
        fun f(a: String, b: Int = 5): Int = a.length + b\n\
        fun g(): Int = f(\"hi\")\n\
    }\n\
    fun box(): String {\n\
        val c = C()\n\
        return if (c.g() == 7 && c.f(\"hi\", 10) == 12) \"OK\" else \"FAIL:${c.g()}:${c.f(\"hi\", 10)}\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("implicit-receiver omitted default + runs"),
        "OK"
    );
}

#[test]
fn forward_reference_to_constant_default_member() {
    // The CALLER (`caller`) is defined BEFORE the defaulted `target` — the forward-reference case that
    // saw only the empty pass-1 marker. `null` (a constant) must be filled for the omitted `tag`.
    const SRC: &str = "class C {\n\
        fun caller(): Int = target(\"x\")\n\
        fun target(a: String, tag: String? = null): Int = a.length + (tag?.length ?: 0)\n\
    }\n\
    fun box(): String {\n\
        val c = C()\n\
        return if (c.caller() == 1 && c.target(\"x\", \"yz\") == 3) \"OK\" else \"FAIL:${c.caller()}:${c.target(\"x\", \"yz\")}\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("forward-reference constant default + runs"),
        "OK"
    );
}
