//! Kotlin metadata lists a type parameter's bounds in the order the declaration writes them, while
//! the JVM generic signature and erasure lead with the class bound. `where T : Marker, T : Base`
//! records `Marker` then `Base` in metadata and `<T:LBase;:LMarker;>` in the signature; a
//! class-first declaration records `Base` then `Marker` in both.
use super::common;

const SOURCE: &str = "package bounds\n\
                      \n\
                      interface Marker\n\
                      \n\
                      open class Base\n\
                      \n\
                      class Both : Base(), Marker\n\
                      \n\
                      fun <T> interfaceFirst(value: T): String where T : Marker, T : Base = \"interface\"\n\
                      \n\
                      fun <T> classFirst(value: T): String where T : Base, T : Marker = \"class\"\n\
                      \n\
                      val <T> T.markedFirst: String where T : Marker, T : Base\n\
                      \x20   get() = \"marked\"\n\
                      \n\
                      class Holder<T> where T : Marker, T : Base {\n\
                      \x20   fun <U> pick(value: U): String where U : Marker, U : Base = \"pick\"\n\
                      }\n\
                      \n\
                      fun box(): String {\n\
                      \x20   val both = Both()\n\
                      \x20   if (interfaceFirst(both) != \"interface\") return \"interfaceFirst\"\n\
                      \x20   if (classFirst(both) != \"class\") return \"classFirst\"\n\
                      \x20   if (both.markedFirst != \"marked\") return \"markedFirst\"\n\
                      \x20   return if (Holder<Both>().pick(both) == \"pick\") \"OK\" else \"pick\"\n\
                      }\n";

#[test]
fn intersection_bounds_keep_their_written_order_in_metadata() {
    for class in [
        "bounds/IntersectionBoundsKt",
        "bounds/Holder",
        "bounds/Marker",
        "bounds/Base",
        "bounds/Both",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "IntersectionBounds",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

#[test]
fn intersection_bound_declarations_run() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SOURCE, "IntersectionBounds"),
        "OK"
    );
}

const LIBRARY: &str = "package lib\n\
                       \n\
                       interface Marker\n\
                       \n\
                       open class Base\n\
                       \n\
                       class Both : Base(), Marker\n\
                       \n\
                       fun <T> choose(value: T): String where T : Marker = \"marker\"\n\
                       \n\
                       fun <T> choose(value: T): String where T : Marker, T : Base = \"base\"\n";

/// A kotlinc-compiled dependency records the interface bound first, yet the second `choose`
/// erases to its class bound `Base`: the descriptor a metadata function aligns with is its primary
/// bound's, not its first written bound's.
#[test]
fn a_dependency_function_erases_an_interface_first_bound_to_its_class() {
    let main = "import lib.*\n\
                \n\
                fun box(): String = if (choose(Both()) == \"base\") \"OK\" else \"fail\"\n";
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIBRARY, main)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}

const OWNER_BOUNDS: &str = "package owner\n\
                            \n\
                            open class A\n\
                            \n\
                            open class B\n\
                            \n\
                            class C<T : A, U : B> {\n\
                            \x20   fun choose(a: T): String = \"A\"\n\
                            \n\
                            \x20   fun choose(b: U): String = \"B\"\n\
                            }\n";

/// Each overload is called by its own parameter name, which only its metadata function declares.
const OWNER_BOUND_CALLS: &str = "import owner.*\n\
                                 \n\
                                 fun box(): String {\n\
                                 \x20   val c = C<A, B>()\n\
                                 \x20   val positional = c.choose(A()) + c.choose(B())\n\
                                 \x20   val named = c.choose(a = A()) + c.choose(b = B())\n\
                                 \x20   return if (positional == \"AB\" && named == \"AB\") \"OK\" else \"$positional $named\"\n\
                                 }\n";

/// A kotlinc-built class's overloads that differ only in which of its own type parameters they
/// take erase to that parameter's bound, `choose(LA;)` and `choose(LB;)`. Each call, positional or
/// named, links the overload it selected with that overload's parameter.
#[test]
fn dependency_overloads_over_enclosing_class_parameters_are_called_like_kotlinc() {
    let results = common::class_bytes_diffs_against_kotlinc(
        "OwnerBoundCalls",
        &[("OwnerBounds.kt", OWNER_BOUNDS)],
        OWNER_BOUND_CALLS,
        "OwnerBoundCallsKt",
        &["box"],
    )
    .expect("reference kotlinc is provisioned");
    for result in results {
        result.unwrap_or_else(|diff| panic!("{diff}"));
    }
    assert_eq!(
        common::expect_box_run_against_kotlinc(OWNER_BOUNDS, OWNER_BOUND_CALLS)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
