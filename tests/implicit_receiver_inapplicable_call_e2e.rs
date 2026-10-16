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
