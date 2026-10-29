//! A primitive receiver of a library inline function's type parameter is boxed into the erased slot.
//!
//! `also` and `apply` declare `T.()`/`(T)` over an unconstrained `T`, so kotlinc stores the receiver
//! as `Object`: a `Boolean`, `Int` or `Long` is boxed at the store and unboxed for the lambda. A
//! result that is consumed unboxed keeps the primitive, since every use of the box then unboxes.

use super::common;

const SOURCE: &str = r#"val log = StringBuilder()

fun flag(b: Boolean): Any? = b.also { log.append(it) }
fun negated(b: Boolean): Any? = (!b).also { log.append(it) }
fun explicit(b: Boolean): Any? = b.also<Boolean> { log.append(it) }
fun kept(b: Boolean): Boolean = b.also { log.append(it) }
fun number(n: Int): Any? = n.also { log.append(it) }
fun wide(n: Long): Any? = n.also { log.append(it) }
fun applied(n: Int): Any? = n.apply { log.append(this) }
fun text(s: String): Any? = s.also { log.append(it) }

fun box(): String {
    val results = listOf(flag(true), negated(true), explicit(false), kept(true), number(4), wide(5L), applied(6), text("x"))
    return if (results.toString() == "[true, false, false, true, 4, 5, 6, x]" && log.toString() == "truefalsefalsetrue456x") "OK" else "Fail $results $log"
}
"#;

#[test]
fn primitive_receiver_of_library_inline_is_boxed_like_kotlinc() {
    common::expect_box_same_as_kotlinc(SOURCE, "InlinePrimitiveReceiver");
    common::assert_classes_identical_to_kotlinc(
        "InlinePrimitiveReceiver",
        SOURCE,
        &["InlinePrimitiveReceiverKt"],
    );
}
