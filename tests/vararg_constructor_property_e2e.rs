//! A vararg constructor property is an array. A body property of the same class is not.
//!
//! Constructor parameters and body properties are both numbered from zero. Wrapping every
//! property at the vararg parameter's index turns `val major: Int` into an `IntArray`.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn a_body_property_keeps_its_type_beside_a_vararg_constructor_property() {
    expect_box_same_as_kotlinc(
        r#"
abstract class Version(private vararg val numbers: Int) {
    val major: Int = numbers.getOrNull(0) ?: -1
    val minor: Int = numbers.getOrNull(1) ?: -1
    abstract fun current(): Boolean
    fun matches(other: Version): Boolean =
        major == other.major && minor == other.minor
}

class Bag(vararg val values: Int) {
    val label: Int = 7
    fun first(): Int = values.getOrNull(0) ?: -1
    fun count(): Int = values.size
}

class Tagged(val tag: Int, vararg val parts: Int) {
    val extra: Int = 3
    val more: Int = parts.getOrNull(0) ?: -1
}

fun box(): String {
    val version = object : Version(1, 9) {
        override fun current() = true
    }
    if (version.major != 1 || version.minor != 9) return "version"
    if (!version.matches(version)) return "match"
    val bag = Bag(4, 5)
    if (bag.first() != 4 || bag.count() != 2 || bag.label != 7) return "bag"
    val tagged = Tagged(1, 8)
    if (tagged.tag != 1 || tagged.extra != 3 || tagged.more != 8) return "tagged"
    return "OK"
}
"#,
        "VarargConstructorProperty",
    );
}
