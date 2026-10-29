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

/// A value still in the erased slot reaches a primitive field, a constructor default, and a
/// function default unboxed.
const STORES: &str = r#"val objects = ArrayList<Any>()
fun log(o: Any) { objects.add(o) }

class Field(val num: Int) {
    val map: Int = 1.apply { log(num) }
}
class Defaulted(val x: Int = 3.also(::log), val y: Long = 4L.also { log(it) })
fun defaulted(x: Int = 5.also(::log), flag: Boolean = true.also { log(it) }): String = "$x$flag"

fun box(): String {
    val field = Field(2).map
    val instance = Defaulted()
    val call = defaulted()
    val result = "$field ${instance.x} ${instance.y} $call $objects"
    return if (result == "1 3 4 5true [2, 3, 4, 5, true]") "OK" else "Fail $result"
}
"#;

#[test]
fn stores_of_an_erased_primitive_receiver_unbox() {
    common::expect_box_same_as_kotlinc(STORES, "InlinePrimitiveReceiverStores");
}
