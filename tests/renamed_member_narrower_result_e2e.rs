//! A member that overrides a renamed builtin member with a narrower result, declared abstract or
//! not, gets kotlinc's two bridges: the final special bridge under the JVM name at the member's own
//! result, then an ordinary synthetic bridge at the declaration's erased descriptor that calls the
//! special one and returns its result uncast (`removeAt(int): String` gives `final String
//! remove(int)` and `Object remove(int)`). A generic member keeps one special bridge that carries
//! its generic signature (`final T remove(int)`). Each class is compared byte for byte with
//! kotlinc's, and Java's `java.util.List.remove(int)` reaches the member.

use super::common;

const ROWS: &str = "abstract class AnyRow : MutableList<Any> {
    abstract override fun removeAt(index: Int): String
}

abstract class TextRow : MutableList<String> {
    abstract override fun removeAt(index: Int): String
}

abstract class GenericRow<T> : MutableList<T> {
    abstract override fun removeAt(index: Int): T
}

abstract class ConcreteAnyRow : MutableList<Any> {
    override fun removeAt(index: Int): String = \"removed $index\"
}

abstract class ConcreteTextRow : MutableList<String> {
    override fun removeAt(index: Int): String = \"removed $index\"
}
";

#[test]
fn a_narrower_renamed_member_gets_both_bridges_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "RenamedMemberResults",
        ROWS,
        &[
            "AnyRow",
            "TextRow",
            "GenericRow",
            "ConcreteAnyRow",
            "ConcreteTextRow",
        ],
    );
}

#[test]
fn java_reaches_a_narrower_renamed_member_like_kotlinc() {
    let program = format!(
        "{ROWS}
class Items(private val items: MutableList<Any> = mutableListOf(\"a\", \"b\")) : AnyRow(), MutableList<Any> by items {{
    override fun removeAt(index: Int): String = \"removed \" + items.removeAt(index)
}}

fun box(): String {{
    val remove = java.util.List::class.java.getMethod(\"remove\", Int::class.javaPrimitiveType)
    val outcome = remove.invoke(Items(), 1)
    return if (outcome == \"removed b\") \"OK\" else \"$outcome\"
}}
"
    );
    common::expect_box_same_as_kotlinc(&program, "renamed_member_narrower_result");
}
