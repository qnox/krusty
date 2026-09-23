//! A property initialized to an UNSIGNED zero drops its declaration store, as one initialized to
//! any other JVM default does.
//!
//! kotlinc omits a declaration store whose value the field already holds. That is observable, not
//! only a size saving: a base constructor that dispatches to an override runs BEFORE the subclass's
//! initializers, so a value the override wrote survives when the store is omitted and is wiped when
//! it is kept. `0u`, `0uL`, `0.toUByte()` and `0.toUShort()` are the carrier's zero like `0` is, and
//! were the only zeros krusty kept.

use super::common;

#[test]
fn an_unsigned_zero_initializer_keeps_what_a_base_constructor_wrote() {
    const SRC: &str = "open class Base {\n\
    init { write() }\n\
    open fun write() {}\n\
}\n\
class Derived : Base() {\n\
    var i: UInt = 0u\n\
    var l: ULong = 0uL\n\
    var b: UByte = 0u\n\
    var s: UShort = 0u\n\
    var plain: Int = 0\n\
    override fun write() {\n\
        i = 5u\n\
        l = 6uL\n\
        b = 7u\n\
        s = 8u\n\
        plain = 9\n\
    }\n\
}\n\
fun box(): String {\n\
    val d = Derived()\n\
    return \"\" + d.i + \" \" + d.l + \" \" + d.b + \" \" + d.s + \" \" + d.plain\n\
}\n";
    let reference = common::kotlinc_box_result(SRC);
    let krusty = common::expect_box_run_with_stdlib(SRC, "UnsignedDefaultStore");
    assert_eq!(krusty, reference, "krusty and kotlinc disagree");
    assert_eq!(krusty, "5 6 7 8 9");
}
