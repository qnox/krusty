//! A private companion `lateinit` property is read from the outer class through a raw field
//! bridge. The uninitialized guard still belongs on that read: a missing one returns null.

use super::common::expect_box_same_as_kotlinc;

/// `properties/lateinit/accessorException.kt`.
#[test]
fn companion_lateinit_read_from_the_outer_class_throws() {
    expect_box_same_as_kotlinc(
        r#"
public class A {
    fun getFromClass(): Boolean {
        try {
            val a = str
            return false
        } catch (e: RuntimeException) {
            return true
        }
    }

    fun getFromCompanion() = Companion.getFromCompanion()

    private companion object {
        private lateinit var str: String

        fun getFromCompanion(): Boolean {
            try {
                val a = str
                return false
            } catch (e: RuntimeException) {
                return true
            }
        }
    }
}

fun box(): String {
    if (!A().getFromClass()) return "Fail getFromClass"
    if (!A().getFromCompanion()) return "Fail getFromCompanion"

    return "OK"
}
"#,
        "LateinitCompanionRead",
    );
}

/// The bridge read is branchy, so an earlier operand must be spilled before the guard's join.
#[test]
fn companion_lateinit_read_spills_an_earlier_operand() {
    expect_box_same_as_kotlinc(
        r#"
class Outer {
    fun read(prefix: String) = prefix + str

    fun go(): String {
        Companion.init()
        return read("O")
    }

    private companion object {
        private lateinit var str: String
        fun init() { str = "K" }
    }
}

fun box(): String {
    val value = Outer().go()
    return if (value == "OK") "OK" else "fail: $value"
}
"#,
        "LateinitCompanionOperand",
    );
}
