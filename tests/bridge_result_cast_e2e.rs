//! kotlinc's bridge materializes the delegated result at the bridge's own return type, and
//! `StackValue.coerce` casts between two different reference types unless the target is `Object`
//! or an array: a `String` override bridged to a `CharSequence` declaration returns through a
//! `checkcast CharSequence`, while the `Object` and `Array<out Any>` bridges return the result as
//! it is.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      interface Source { fun text(): CharSequence; fun items(): Array<out Any>; fun any(): Any; fun num(): Number }\n\
                      \n\
                      open class Base<T>(val v: T) { open fun get(): T = v }\n\
                      \n\
                      interface Named { fun get(): String }\n\
                      \n\
                      class Impl : Source {\n\
                      \x20   override fun text(): String = \"a\"\n\
                      \x20   override fun items(): Array<String> = arrayOf(\"x\")\n\
                      \x20   override fun any(): String = \"b\"\n\
                      \x20   override fun num(): Int = 1\n\
                      }\n\
                      \n\
                      class Diamond : Base<String>(\"s\"), Named\n\
                      \n\
                      abstract class Comparer : Comparable<Comparer> {\n\
                      \x20   abstract fun self(): Comparable<Comparer>\n\
                      }\n\
                      \n\
                      class Sub : Comparer() {\n\
                      \x20   override fun compareTo(other: Comparer): Int = 0\n\
                      \x20   override fun self(): Sub = this\n\
                      }\n";

#[test]
fn bridge_results_are_cast_like_kotlinc() {
    for class in ["store/Impl", "store/Diamond", "store/Sub"] {
        common::byte_diff_against_kotlinc_cp(
            "BridgeResultCast",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
