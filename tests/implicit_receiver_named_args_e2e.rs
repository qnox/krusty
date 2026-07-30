//! Named arguments on a call through a SMART-CAST implicit receiver — `when (this) { is B -> copy(x = v) }`.
//!
//! An implicit receiver is sugar for `this.`, so a member reached after `this` is narrowed by an `is`
//! check takes named arguments exactly as the qualified form does. Two layers disagreed:
//!
//! - The checker's `supports_named` predicate asked `implicit_receiver_types()`, which reports the
//!   DECLARED receiver. In `fun Op.f() = when (this) { is Create -> copy(path = p) }` that is `Op`, which
//!   has no `copy` to take the labels from, so the call was rejected before the narrowed-this resolution
//!   that would have found `Create.copy` ever ran.
//! - Lowering then re-derived the call itself with a positional argument loop, which can neither reorder
//!   for a named argument nor emit `<name>$default(…, mask, marker)` for an omitted default — so the same
//!   source bailed the whole file at the IR backend.
//!
//! Both now go through the seams the qualified call already used: `effective_this_narrow` on the checker
//! side, `lower_module_member_call` on the lowering side. The explicitly qualified `this.copy(path = p)`
//! and the positional `copy(p, n)` both already worked, which is what localized the gap.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

/// The reported shape: a named argument that also OMITS a defaulted parameter, on a `copy` reached
/// through a smart-cast `this`. Needs the reorder AND the `$default` synthetic, which is why the
/// positional fallback could not serve it.
#[test]
fn a_named_argument_omitting_a_default_through_a_smart_cast_this() {
    const SRC: &str = "sealed interface Op\n\
    data class Create(val path: String, val n: Int = 7) : Op\n\
    fun Op.renamed(p: String): Op =\n\
    \x20   when (this) {\n\
    \x20       is Create -> copy(path = p)\n\
    \x20       else -> this\n\
    \x20   }\n\
    fun box(): String {\n\
    \x20   val r = Create(\"a\", 1).renamed(\"b\") as Create\n\
    \x20   return r.path + r.n\n\
    }\n";
    assert_eq!(
        run(SRC).expect("named argument omitting a default through a smart-cast `this`"),
        "b1"
    );
}

/// A named argument that REORDERS through the same receiver. Source order must not leak into the call:
/// with both parameters supplied out of order, a positional lowering would swap them.
#[test]
fn a_reordering_named_argument_through_a_smart_cast_this_matches_kotlinc() {
    const SRC: &str = "sealed interface Op\n\
    data class Create(val a: String, val b: String) : Op\n\
    fun Op.mixed(): String =\n\
    \x20   when (this) {\n\
    \x20       is Create -> copy(b = \"Y\", a = \"X\").a + copy(b = \"Y\", a = \"X\").b\n\
    \x20       else -> \"none\"\n\
    \x20   }\n\
    fun box(): String = Create(\"a\", \"b\").mixed()\n";
    assert_eq!(
        run(SRC).expect("reordered named arguments through a smart-cast `this`"),
        "XY"
    );
}

/// A named argument on an ordinary (non-`copy`) member of the narrowed type — the fix is about the
/// receiver, not about data-class `copy` in particular.
#[test]
fn a_named_argument_on_a_plain_member_of_the_narrowed_type() {
    const SRC: &str = "sealed interface Op\n\
    class Create(val tag: String) : Op {\n\
    \x20   fun mix(a: String, b: String): String = tag + a + b\n\
    }\n\
    fun Op.mixed(): String =\n\
    \x20   when (this) {\n\
    \x20       is Create -> mix(b = \"Y\", a = \"X\")\n\
    \x20       else -> \"none\"\n\
    \x20   }\n\
    fun box(): String = Create(\"t\").mixed()\n";
    assert_eq!(
        run(SRC).expect("named arguments on a plain member of the narrowed type"),
        "tXY"
    );
}

/// The two forms that ALREADY worked, kept as regression guards: the fix must not disturb the explicitly
/// qualified call or the positional one through the same narrowed receiver. Both were the control that
/// localized the gap to the implicit-plus-named combination.
#[test]
fn the_explicit_and_positional_forms_still_work() {
    const EXPLICIT: &str = "sealed interface Op\n\
    data class Create(val path: String, val n: Int = 7) : Op\n\
    fun Op.renamed(p: String): Op =\n\
    \x20   when (this) {\n\
    \x20       is Create -> this.copy(path = p)\n\
    \x20       else -> this\n\
    \x20   }\n\
    fun box(): String = (Create(\"a\", 1).renamed(\"b\") as Create).path\n";
    assert_eq!(run(EXPLICIT).expect("explicit `this.copy(path = …)`"), "b");

    const POSITIONAL: &str = "sealed interface Op\n\
    data class Create(val path: String, val n: Int = 7) : Op\n\
    fun Op.renamed(p: String): Op =\n\
    \x20   when (this) {\n\
    \x20       is Create -> copy(p, n)\n\
    \x20       else -> this\n\
    \x20   }\n\
    fun box(): String = (Create(\"a\", 1).renamed(\"b\") as Create).path\n";
    assert_eq!(run(POSITIONAL).expect("positional `copy(p, n)`"), "b");
}
