//! A bare call that names only members of an implicit receiver, none of which accepts the
//! arguments, is an inapplicable call of that member, not an unresolved reference: kotlinc reports
//! the argument type mismatch of the one candidate the arguments map onto, even when the receiver
//! also inherits a same-named function of another arity (`Any.notify()`). A parameter that names
//! the enclosing class's type parameter keeps it in the message, through `this` or without it.

use super::common;

#[test]
fn an_inapplicable_implicit_receiver_member_reports_its_argument_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            "class Generic<T> {\n\
             \x20   private fun notify(xs: List<T>) {}\n\
             \x20   fun use(xs: List<T>?) { notify(xs) }\n\
             }\n\
             class Single {\n\
             \x20   private fun take(xs: List<Int>) {}\n\
             \x20   fun use(xs: List<Int>?) { take(xs) }\n\
             }\n\
             open class Base { fun take() {} }\n\
             class Derived : Base() {\n\
             \x20   fun take(xs: List<Int>) {}\n\
             \x20   fun use(xs: List<Int>?) { take(xs) }\n\
             }\n\
             class Overloads {\n\
             \x20   fun take() {}\n\
             \x20   fun take(xs: List<Int>) {}\n\
             \x20   fun use(xs: List<Int>?) { take(xs) }\n\
             }\n\
             class Bounded<T : CharSequence> {\n\
             \x20   fun take(xs: List<T>) {}\n\
             \x20   fun use(xs: List<T>?) { this.take(xs) }\n\
             }\n",
        )],
        &[],
    );
}

/// A body-local classifier's member family exists only in the active body-checking overlay. The
/// rejected call must therefore diagnose from the family selected at that receiver rung; querying
/// the immutable module provider afterward would lose the declaration and report an unresolved
/// name instead.
#[test]
fn a_rejected_body_local_member_uses_the_retained_candidate_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            "fun enclosing() {\n\
             \x20   class Local {\n\
             \x20       fun take(xs: List<Int>) {}\n\
             \x20       fun use(xs: List<Int>?) { take(xs) }\n\
             \x20   }\n\
             }\n",
        )],
        &[],
    );
}

/// The explicit-receiver path must consume the same retained body-local candidate family. The
/// immutable module provider cannot reproduce a local classifier's methods after selection.
#[test]
fn an_explicit_receiver_rejection_uses_the_retained_body_local_candidate_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            "fun enclosing(xs: List<Int>?) {\n\
             \x20   class Local { fun take(values: List<Int>) {} }\n\
             \x20   Local().take(xs)\n\
             }\n",
        )],
        &[],
    );
}

/// Safe calls enter the same retained-candidate diagnostic path through separate `String` and
/// classifier receiver branches. The nullable body-local receiver must keep its local declaration,
/// while the `String` branch must keep the stdlib extension family; both errors and their order are
/// compared exactly with kotlinc.
#[test]
fn safe_call_rejections_retain_string_and_body_local_candidates_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            "fun enclosing(xs: List<Int>?) {\n\
             \x20   val text: String? = \"text\"\n\
             \x20   text?.take(xs)\n\
             \x20   class Local { fun take(values: List<Int>) {} }\n\
             \x20   val local: Local? = Local()\n\
             \x20   local?.take(xs)\n\
             }\n",
        )],
        &[],
    );
}
