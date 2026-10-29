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

/// A primitive operation or store consuming the boxed receiver unboxes it at the read: negation,
/// arithmetic, a comparison, `!`, a top-level property assignment and a local reassignment.
const OPERATORS: &str = r#"val log = StringBuilder()
var count = 0

fun negated(n: Int): Int = -(n.also { log.append(it) })
fun added(n: Int): Int = n.also { log.append(it) } + 2
fun multiplied(n: Long): Long = n.apply { log.append(this) } * 3L
fun inverted(b: Boolean): Boolean = !b.also { log.append(it) }
fun counted(n: Int) { count = n.also { log.append(it) } }
fun reassigned(n: Int): Int { var x = 0; x = n.also { log.append(it) }; return x * 3 }
fun compared(n: Int): Boolean = n.also { log.append(it) } > 4

fun box(): String {
    val values = listOf(negated(1), added(2), multiplied(3L), inverted(true), reassigned(5), compared(6))
    counted(4)
    val result = "$values $count $log"
    return if (result == "[-1, 4, 9, false, 15, true] 4 123true564") "OK" else "Fail $result"
}
"#;

#[test]
fn primitive_consumers_of_a_boxed_receiver_unbox_like_kotlinc() {
    common::expect_box_same_as_kotlinc(OPERATORS, "InlinePrimitiveReceiverOperators");
    common::assert_classes_identical_to_kotlinc(
        "InlinePrimitiveReceiverOperators",
        OPERATORS,
        &["InlinePrimitiveReceiverOperatorsKt"],
    );
}

/// Stores whose classes still differ from kotlinc for reasons outside the receiver's
/// representation run against kotlinc's result: an instance field initializer, a setter call,
/// a compound assignment, and constructor and function defaults.
const STORES: &str = r#"val log = StringBuilder()
var total = 0L

class Field(val num: Int) {
    var sum = 0L
    val map: Int = 1.apply { log.append(num) }
}
class Defaulted(val x: Int = 3.also { log.append(it) }, val y: Long = 4L.also { log.append(it) })
fun defaulted(x: Int = 5.also { log.append(it) }, flag: Boolean = true.also { log.append(it) }): String = "$x$flag"
fun summed(n: Long) { total += n.also { log.append(it) } }
fun stashed(field: Field, n: Long) { field.sum = n.also { log.append(it) } }

fun box(): String {
    val field = Field(2)
    stashed(field, 9L)
    summed(6L)
    val instance = Defaulted()
    val call = defaulted()
    val result = "${field.map} ${field.sum} $total ${instance.x} ${instance.y} $call $log"
    return if (result == "1 9 6 3 4 5true 296345true") "OK" else "Fail $result"
}
"#;

#[test]
fn stores_of_a_boxed_receiver_unbox() {
    common::expect_box_same_as_kotlinc(STORES, "InlinePrimitiveReceiverStores");
}
