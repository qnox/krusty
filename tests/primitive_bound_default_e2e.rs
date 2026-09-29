//! A non-null type parameter bounded by a JVM primitive is that primitive on the real method
//! and the JDK wrapper on the `$default` stub. `T : Char` therefore calls
//! `test$nested$default(Test, Character, int, Object)` with `Character.valueOf`, and the stub
//! unboxes with `charValue` before the real `char` method. The same boundary holds for a member.

use super::common;

const LOCAL: &str = "\
class Test<T : Char>(val k: T) {\n\
    fun test(): String {\n\
        fun nested(x: T = k): String = \"O$x\"\n\
        return nested()\n\
    }\n\
}\n\
fun box(): String = Test('K').test()\n\
";

const MEMBER: &str = "\
class Test<T : Char>(val k: T) {\n\
    fun nested(x: T = k): String = \"O$x\"\n\
    fun test(): String = nested()\n\
}\n\
fun box(): String = Test('K').test()\n\
";

fn assert_same_instructions(stem: &str, src: &str, class: &str, member: &str) {
    let built =
        common::compare_with_kotlinc_plugin(stem, src, class, &[common::stdlib_jar()], "25", &[])
            .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

#[test]
fn a_local_primitive_bound_default_boxes_like_kotlinc() {
    assert_same_instructions(
        "PrimitiveBoundLocalDefault",
        LOCAL,
        "Test",
        "java.lang.String test();",
    );
    assert_same_instructions(
        "PrimitiveBoundLocalDefault",
        LOCAL,
        "Test",
        "java.lang.String test$nested$default(Test, java.lang.Character, int, java.lang.Object);",
    );
}

#[test]
fn a_member_primitive_bound_default_boxes_like_kotlinc() {
    assert_same_instructions(
        "PrimitiveBoundMemberDefault",
        MEMBER,
        "Test",
        "java.lang.String test();",
    );
    assert_same_instructions(
        "PrimitiveBoundMemberDefault",
        MEMBER,
        "Test",
        "java.lang.String nested$default(Test, java.lang.Character, int, java.lang.Object);",
    );
}

#[test]
fn a_primitive_bound_default_runs() {
    common::expect_box_ok_with_stdlib(LOCAL, "PrimitiveBoundLocalDefault");
    common::expect_box_ok_with_stdlib(MEMBER, "PrimitiveBoundMemberDefault");
}
