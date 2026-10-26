//! Where a value class's lambdas and local functions go among its members, against kotlinc.
//!
//! kotlinc's inline-class lowering adds the private primary `<init>` and the representation
//! members (`constructor-impl`, `box-impl`, `unbox-impl`, `equals-impl0`) after the declared
//! members and the `Any` overrides. The methods lifted out of member bodies, a local function's
//! `m_impl$local` and an indy lambda's `m_impl$lambda$0`, come after all of those, ahead of the
//! static initializer. krusty placed them after the last declared member, ahead of the boxed `Any`
//! overrides and `<init>`.

use super::common;

const SOURCE: &str = r#"@JvmInline
value class Text(val v: String) {
    fun lazyCopy(): Lazy<String> = lazy { v + "!" }

    fun measured(): Int {
        fun plus(x: Int) = x + v.length
        return plus(1)
    }

    fun counter(): () -> Int = { v.length }
}

fun box(): String {
    val text = Text("ab")
    val got = text.lazyCopy().value + text.measured() + text.counter()()
    return if (got == "ab!32") "OK" else got
}
"#;

#[test]
fn lifted_members_follow_the_representation_members() {
    common::assert_classes_identical_to_kotlinc("ValueClassLiftedMembers", SOURCE, &["Text"]);
}

#[test]
fn lifted_members_run() {
    common::expect_box_ok_with_stdlib(SOURCE, "ValueClassLiftedMembers");
}
