//! A companion property whose accessor mentions `field` keeps that field when plainer companion
//! properties are moved to the outer class. The field table is then compacted, and the property
//! that stayed must still name the field that remains: a `var` that only customizes its setter
//! still publishes the default getter beside that setter.

use super::common::expect_box_same_as_kotlinc;

/// `properties/classObjectProperties.kt`: `prop7` has a custom setter and no getter. A read is the
/// default getter of the stored field; the setter ignores its argument and increments the field.
#[test]
fn companion_var_with_only_a_custom_setter_keeps_its_getter() {
    expect_box_same_as_kotlinc(
        r#"
import kotlin.reflect.KProperty

class Delegate {
    operator fun getValue(thisRef: Any?, property: KProperty<*>): String = "delegated"
}

class ExtensionDelegate {
    operator fun getValue(receiver: Int, property: KProperty<*>): String = "extension"
}

class Test {
    companion object {
        public val prop1: Int = 10
        public var prop2: Int = 11
            private set
        public val prop3: Int = 12
            get() {
                return field
            }
        var prop4: Int = 13
        fun incProp4() {
            prop4++
        }
        public var prop5: Int = 14
        val delegated: String by Delegate()
        val Int.extension: String by ExtensionDelegate()
        fun readExtension(): String = 1.extension
        public var prop7: Int = 20
            set(i: Int) {
                field++
            }
    }
}

fun box(): String {
    val t = Test
    if (t.prop1 != 10) return "fail1"
    if (t.prop2 != 11) return "fail2"
    if (t.prop3 != 12) return "fail3"
    if (t.prop4 != 13) return "fail4"
    t.incProp4()
    if (t.prop4 != 14) return "fail4.inc"
    if (t.prop5 != 14) return "fail5"
    t.prop5 = 1414
    if (t.prop5 != 1414) return "fail6"
    if (t.delegated != "delegated") return "fail.delegated"
    if (t.readExtension() != "extension") return "fail.extension"
    if (t.prop7 != 20) return "fail7"
    t.prop7 = 1000000
    if (t.prop7 != 21) return "fail8"
    return "OK"
}
"#,
        "CompanionCustomSetter",
    );
}
