//! kotlinc writes a type parameter's bounds, and everything below a supertype's own arguments, in
//! its generic-argument mode: every declaration-site variance becomes a wildcard at every depth,
//! even one a parameter position drops as redundant (`out` over a final class, `in` over `Any`).
//! A function-type bound is an interface bound, so its class bound is left empty (`T::`).
use super::common;

const BOUNDS: &str = r##"interface Source<out X>
interface Sink<in X>
interface Inv<X>
class Leaf
fun <T : Source<Leaf>> finalElement(t: T) {}
fun <T : Sink<Any>> overAny(t: T) {}
fun <T : Inv<Sink<Source<Leaf>>>> belowInvariant(t: T) {}
fun <T : Source<Source<Int>>> nested(t: T) {}
fun <T : Sink<Sink<Int>>> nestedSink(t: T) {}
fun <T : (Any) -> Int> function(t: T) {}
fun <T> constrained(t: T) where T : () -> String {}
fun <T : Source<Int>> returned(t: T): Source<Int> = t
"##;

#[test]
fn type_parameter_bounds_write_every_wildcard_like_kotlinc() {
    common::byte_diff_against_kotlinc("GenericArgumentBounds", BOUNDS, "GenericArgumentBoundsKt")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_class_type_parameter_bound_writes_every_wildcard_like_kotlinc() {
    const SRC: &str = "interface Source<out X>\nclass Holder<T : Source<Int>, U : (Int) -> Int>\n";
    common::byte_diff_against_kotlinc("GenericArgumentClassBounds", SRC, "Holder")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const SUPERTYPES: &str = r##"interface Source<out X>
interface Sink<in X>
interface Inv<X>
abstract class FinalElement : Inv<Source<Int>>
abstract class OverAny : Inv<Sink<Any>>
"##;

#[test]
fn arguments_below_a_supertype_write_every_wildcard_like_kotlinc() {
    for class in ["FinalElement", "OverAny"] {
        common::byte_diff_against_kotlinc("GenericArgumentSupertypes", SUPERTYPES, class)
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class}: {diff}"));
    }
}
