//! A delegated property of a generic class types its accessor with the class's type parameter, so
//! the getter carries the `()TC;` Signature kotlinc writes for it.

use super::common;

const SOURCE: &str = r#"interface Context<C : Any> {
    val value: C
}

class Lazy<C : Any>(val getValue: () -> C) : Context<C> {
    override val value: C by lazy(getValue)
}

class Local<C : Any>(initial: C) {
    val value: C by lazy { initial }
}
"#;

#[test]
fn a_delegated_property_getter_in_a_generic_class_is_signed_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "DelegatedPropertyAccessorSignature",
        SOURCE,
        &["Lazy", "Local"],
    );
}
