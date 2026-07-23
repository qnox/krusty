//! A property declared as a bare type parameter (`class Box<T: Int>(val value: T)`) read through a
//! receiver whose type ARGUMENTS were not recorded (`Box(41).value`) types at the parameter's BOUND
//! erasure, not `Any` — so arithmetic on a primitive-bounded read and a CHAINED read through a
//! class-bounded parameter both resolve.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn primitive_bound_property_read() {
    const SRC: &str = "class Box<T: Int>(val value: T)\n\
fun box(): String {\n\
    val a = Box(41).value + 1\n\
    return if (a == 42) \"OK\" else \"FAIL: $a\"\n\
}\n";
    assert_eq!(run(SRC).expect("primitive bound property read"), "OK");
}

#[test]
fn class_bound_chained_property_read() {
    const SRC: &str = "class Box<T: Int>(val value: T)\n\
class Chain<T: Box<Int>>(val value: T)\n\
fun box(): String {\n\
    val b = Chain(Box(7)).value.value\n\
    return if (b == 7) \"OK\" else \"FAIL: $b\"\n\
}\n";
    assert_eq!(run(SRC).expect("class bound chained read"), "OK");
}

#[test]
fn reference_bound_member_call_on_read() {
    // The bound is a reference type with members — a read typed at the bound dispatches its members.
    const SRC: &str = "open class Base { fun tag(): String = \"B\" }\n\
class Holder<T: Base>(val value: T)\n\
fun box(): String {\n\
    val t = Holder(Base()).value.tag()\n\
    return if (t == \"B\") \"OK\" else \"FAIL: $t\"\n\
}\n";
    assert_eq!(run(SRC).expect("reference bound member call"), "OK");
}

#[test]
fn explicit_type_argument_still_wins() {
    // An EXPLICIT instantiation records the type argument — substitution beats the bound fallback.
    const SRC: &str = "open class Base { open fun tag(): String = \"B\" }\n\
class Sub : Base() { override fun tag(): String = \"S\" }\n\
class Holder<T: Base>(val value: T)\n\
fun box(): String {\n\
    val h: Holder<Sub> = Holder(Sub())\n\
    val t = h.value.tag()\n\
    return if (t == \"S\") \"OK\" else \"FAIL: $t\"\n\
}\n";
    assert_eq!(run(SRC).expect("explicit targ wins"), "OK");
}
