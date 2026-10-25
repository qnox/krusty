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
