//! A private companion `lateinit` property is read from the outer class through a raw field
//! bridge. The uninitialized guard still belongs on that read: a missing one returns null.

use super::common;

/// `properties/lateinit/accessorException.kt`.
#[test]
fn companion_lateinit_read_from_the_outer_class_throws() {
    common::expect_box_same_as_kotlinc(
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
    common::expect_box_same_as_kotlinc(
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

/// A companion `lateinit` probe follows the field when JVM storage is hoisted onto the outer
/// class. It remains a raw null test rather than a guarded property read.
#[test]
fn companion_lateinit_initialization_probe_reads_the_hoisted_static() {
    const SOURCE: &str = r#"
class Holder {
    companion object {
        lateinit var value: String
        fun initialized(): Boolean = ::value.isInitialized
    }
}

fun box(): String {
    if (Holder.initialized()) return "initialized"
    Holder.value = "OK"
    return if (Holder.initialized()) Holder.value else "not initialized"
}
"#;

    super::common::assert_class_code_matches_kotlinc(
        "CompanionLateinitProbe",
        SOURCE,
        "Holder$Companion",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "CompanionLateinitProbeRun");
}

/// A public property with a private setter still has a private hoisted field. The companion's
/// public getter therefore reads through the outer class's bridge instead of issuing an illegal
/// cross-class `getstatic` of that field.
#[test]
fn public_companion_lateinit_with_private_setter_reads_through_the_outer_bridge() {
    const SOURCE: &str = r#"
class Holder {
    companion object {
        lateinit var value: String
            private set

        fun initialize() {
            value = "OK"
        }
    }
}

fun box(): String {
    Holder.initialize()
    return Holder.value
}
"#;

    for class in ["Holder", "Holder$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "LateinitPrivateSetterRead",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
    common::expect_box_same_as_kotlinc(SOURCE, "LateinitPrivateSetterReadRun");
}
