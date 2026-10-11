//! An `@InlineOnly` extension on a value class is spliced without its lines, locals or SMAP.
//!
//! The metadata names the method by its JVM signature, where the value-class receiver is its
//! carrier (`Object`), so the method, and the privacy that marks it `@InlineOnly`, must be found by
//! that exact signature rather than by the receiver's declared class.

use super::common;

const WRAPPING: &str = r#"@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
package wrapping

@JvmInline
value class Wrapped(val held: Any?)

@kotlin.internal.InlineOnly
inline fun Wrapped.open(): Any? = held
"#;

const OPENING_SOURCE: &str = r#"import wrapping.*

class Holder {
    var value: Any? = null
    fun store(wrapped: Wrapped) {
        value = wrapped.open()
    }
}

fun opened(wrapped: Wrapped): Any? = wrapped.open()
"#;

#[test]
fn an_inline_only_value_class_extension_drops_its_debug_information_like_kotlinc() {
    let library =
        common::kotlinc_library(WRAPPING).expect("reference compiler builds the dependency");
    common::assert_classes_identical_to_kotlinc_against(
        "InlineOnlyValueReceiver",
        OPENING_SOURCE,
        &["Holder", "InlineOnlyValueReceiverKt"],
        &[library],
    );
}
