//! A Java class's javac bridges are ABI, not members, as kotlinc reads a class file. `Integer`'s
//! `compareTo(Object)` bridge made `Int < Char` applicable ahead of a user `Int.compareTo(Char)`
//! operator and failed with a ClassCastException at run time.

use super::common;

#[test]
fn a_user_operator_answers_what_no_primitive_member_takes() {
    let source = "\
        operator fun Int.compareTo(c: Char): Int = if (c == 'O') 1 else -1\n\
        fun above(x: Int, c: Char): Boolean = x > c\n\
        fun box(): String = if (above(42, 'O') && !above(42, 'K')) \"OK\" else \"fail\"\n\
        ";
    common::expect_box_same_as_kotlinc(source, "JavaBridgeMembers");
}
