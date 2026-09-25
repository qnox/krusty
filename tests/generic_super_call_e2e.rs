//! A `super` call dispatches to the selected declaration, so it names that declaration's own
//! erased signature on the supertype it is qualified with, not the signature the call site's
//! generic substitution gives it. With the substituted signature the call fails to link with
//! `NoSuchMethodError`.

use super::common;

#[test]
fn super_call_through_class_that_fixes_a_type_argument() {
    // `Middle` inherits `keep(T, Int)` from `Holder<Int>`: the call is `Middle.keep(Object, int)`,
    // and the `Int` argument is boxed into the `T` slot.
    const SRC: &str = "open class Holder<T> { open fun keep(value: T, extra: Int): T = value }\n\
open class Middle : Holder<Int>()\n\
class Leaf : Middle() {\n\
    override fun keep(value: Int, extra: Int): Int = super.keep(value + extra, extra) * 2\n\
}\n\
fun box(): String = if (Leaf().keep(20, 1) == 42) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(SRC, "Holder");
}

#[test]
fn super_call_to_interface_default_inherited_by_superclass() {
    const SRC: &str = "interface Echo<T> { fun echo(value: T): T = value }\n\
open class Shout : Echo<String>\n\
class Whisper : Shout() {\n\
    override fun echo(value: String): String = super.echo(value).lowercase()\n\
}\n\
fun box(): String = Whisper().echo(\"ok\").uppercase()\n";
    common::expect_box_ok_with_stdlib(SRC, "Echo");
}

#[test]
fn super_call_to_interface_default_through_generic_subinterface() {
    const SRC: &str = "interface Source<K, N : Number, V : Any> {\n\
    fun fetch(key: K, count: N): V? = null\n\
}\n\
interface Feed<K, V : Any> : Source<K, Int, V>\n\
class Reader : Feed<String, Reader> {\n\
    override fun fetch(key: String, count: Int): Reader? = super.fetch(key, count)\n\
}\n\
fun box(): String = if (Reader().fetch(\"\", 0) == null) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(SRC, "Source");
}
