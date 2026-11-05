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

/// A parameter position drops `out` over a final class, but kotlinc never counts an enum as final
/// there: a source enum (with or without entry bodies), a nested one and a Java enum all keep
/// `? extends`, at the top level and below another `out` argument.
const ENUM_ARGUMENTS: &str = r##"import java.util.concurrent.TimeUnit
enum class Plain { A }
enum class WithBodies { A { override fun f() = 1 }; abstract fun f(): Int }
class Box<out T>(val t: T)
class Holder(val nested: List<Holder.Kind>?, val plain: List<Plain>) {
    enum class Kind(val v: String) { X("x") }
}
fun enums(a: List<Plain>, b: List<WithBodies>, c: List<TimeUnit>, d: List<Box<Plain>>, e: List<Box<String>>) {}
"##;

#[test]
fn an_enum_argument_keeps_its_parameter_wildcard_like_kotlinc() {
    for class in ["EnumArgumentsKt", "Holder"] {
        common::byte_diff_against_kotlinc("EnumArguments", ENUM_ARGUMENTS, class)
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class}: {diff}"));
    }
}
