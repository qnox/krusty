//! A lambda invoked inside nested inline calls is spliced, including one copied through each
//! level as an unnamed temporary.

use super::common;

#[test]
fn a_lambda_invoked_inside_nested_inline_forwards_is_byte_identical() {
    let source = r#"class Carrier
class Envelope(val carrier: Carrier)
class Paths

inline fun <T, R> forwardInline(value: T, block: (T) -> R): R = block(value)

inline fun zip(out: Carrier, shuffle: (Paths) -> Unit) {
    forwardInline(out) { carrier ->
        forwardInline(Envelope(carrier)) {
            shuffle(Paths())
        }
    }
}

fun go(out: Carrier) = zip(out) {}
"#;
    common::assert_classes_identical_to_kotlinc("NestedUseLambda", source, &["NestedUseLambdaKt"]);
}
