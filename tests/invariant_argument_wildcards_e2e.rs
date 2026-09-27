//! kotlinc writes declaration-site variance as a wildcard in a parameter's generic `Signature`
//! only while no enclosing argument is invariant. Below an invariant argument (or an array element)
//! it is not written; a contravariant argument below that writes it again for its whole subtree,
//! and an explicit projection that only restates the declared variance is dropped with it. An
//! array's `in` projection reads its elements as `Any?`, so the parameter signs nothing.
use super::common;

const SOURCE: &str = r##"class Box<T>(var v: T)
open class Open

fun invariant(b: Box<List<Any>>) {}
fun invariantFinal(b: Box<Comparable<String>>) {}
fun nestedCovariant(b: Box<List<List<Any>>>) {}
fun reopened(b: Box<Comparable<List<Any>>>) {}
fun reopenedDeep(b: Box<Comparable<List<List<Open>>>>) {}
fun reopenedInvariant(b: Box<Comparable<Box<List<Any>>>>) {}
fun contravariantOuter(b: Comparable<Box<List<Any>>>) {}
fun covariantOuter(b: List<Box<List<Any>>>) {}
fun open(b: Box<List<Open>>, c: Box<Comparable<Open>>) {}
fun restated(b: Box<List<out Any>>, c: Box<Comparable<in Any>>) {}
fun projected(b: Box<Box<out Any>>, c: Box<Box<in Open>>, d: Box<in List<Any>>) {}
fun functions(b: Box<(List<Any>) -> List<Any>>, c: (Box<List<Any>>) -> Unit) {}
fun suspending(b: Box<suspend (List<Any>) -> Unit>) {}
fun arrays(a: Array<List<Any>>, b: Array<out List<Any>>, c: Array<in List<Any>>) {}
fun arrayArguments(a: Box<Array<List<Any>>>, b: Box<Array<out List<Any>>>, c: Array<Comparable<List<Any>>>) {}
fun returned(b: Box<List<Any>>): Box<List<Any>> = b
"##;

#[test]
fn invariant_arguments_drop_declaration_site_wildcards() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::byte_diff_against_kotlinc_cp(
        "InvariantArgumentWildcards",
        SOURCE,
        "InvariantArgumentWildcardsKt",
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|error| panic!("the facade is byte-identical to kotlinc: {error}"));
}

/// A constructor parameter and the field it declares follow the same rule.
#[test]
fn constructor_parameters_follow_the_invariant_rule() {
    let source = "class Box<T>(var v: T)\n\
                  class Holder(val items: Box<List<Any>>, xs: MutableList<List<Any>>)\n";
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::byte_diff_against_kotlinc_cp("InvariantArgumentHolder", source, "Holder", &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|error| panic!("Holder is byte-identical to kotlinc: {error}"));
}
