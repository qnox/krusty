//! The `-impl` static a value class realizes an inherited default with is generic when its carrier
//! is: `isInstanceOf-impl(List<? extends Integer>)` over a `List<Int>` carrier.

use super::common;

const SOURCE: &str = r#"interface Holder<out T> {
    val value: T

    fun isFull(): Boolean = value.toString().isNotEmpty()
}

@JvmInline
value class Numbers(override val value: List<Int>) : Holder<List<Int>>

@JvmInline
value class Plain(override val value: Int) : Holder<Int>
"#;

#[test]
fn an_inherited_default_static_over_a_generic_carrier_is_signed_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "ValueClassInheritedDefaultSignature",
        SOURCE,
        &["Numbers", "Plain"],
    );
}

/// A generic value class's static declares no type parameters of its own, so kotlinc writes the
/// carrier's type arguments as they stand.
#[test]
fn an_inherited_default_static_over_a_type_parameter_carrier_is_signed_like_kotlinc() {
    const GENERIC: &str = r#"interface Holder<out T> {
    val value: T

    fun isFull(): Boolean = value.toString().isNotEmpty()
}

@JvmInline
value class Box<T>(override val value: List<T>) : Holder<List<T>>
"#;
    common::assert_classes_identical_to_kotlinc(
        "ValueClassInheritedDefaultGenericCarrier",
        GENERIC,
        &["Box"],
    );
}
