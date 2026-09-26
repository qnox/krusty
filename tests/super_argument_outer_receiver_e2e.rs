//! A superclass constructor's arguments cannot use the instance under construction.
//!
//! When the enclosing instance a super-constructor argument names is a supertype of the class
//! being constructed, the argument still reads that enclosing instance: `token` in
//! `object : Holder(token)` inside `Holder` is the outer holder's, and the outer `Outer` of
//! `inner class Second : First(s)` is the constructor's outer parameter even though `Second` is
//! itself an `Outer`. krusty read both through the uninitialized `this` and failed verification.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "class Token\n\
open class Holder(val token: Token) {\n\
    fun copy(): Holder = object : Holder(token) {}\n\
}\n\
open class Outer(val s: String) {\n\
    open inner class First(s: String) : Outer(s)\n\
    inner class Second(s: String, extra: Long) : First(s)\n\
}\n";

#[test]
fn super_arguments_read_the_enclosing_instance() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val token = Token()\n\
             \x20   if (Holder(token).copy().token !== token) return \"fail copy\"\n\
             \x20   return Outer(\"fail\").Second(\"OK\", 7L).s\n\
             }}\n"
        ),
        "SuperArgumentOuter",
    );
}

#[test]
fn inner_super_delegation_passes_the_outer_parameter_like_kotlinc() {
    let built = compare_with_kotlinc_plugin(
        "SuperArgumentOuter",
        SOURCE,
        "Outer$Second",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let member = "Outer$Second(Outer, java.lang.String, long)";
    let reference = method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc: {member} not found");
    assert_eq!(method_instructions(&built.krusty, member), reference);
}
