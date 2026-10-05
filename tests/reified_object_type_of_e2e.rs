//! `typeOf` inside an anonymous object's property initializer uses the reified call-site argument.
//!
//! The object is copied once for the inline function that contains it, and again when that function
//! is itself inlined with an outer reified parameter. The declaration class keeps the parameter.
//! The executed class realizes the concrete `KType`.

use super::common;

const SOURCE: &str = "\
import kotlin.reflect.KType\n\
import kotlin.reflect.typeOf\n\
\n\
inline fun <reified T> foo() = object { val x = typeOf<T>() }.x\n\
\n\
inline fun <reified T> bar(expected: KType): String {\n\
    val nested = foo<List<T>>()\n\
    val local = object { val x = typeOf<List<T>>() }.x\n\
    val direct = typeOf<List<T>>()\n\
    if (nested != expected) return \"fail nested=$nested\"\n\
    if (local != expected) return \"fail local=$local\"\n\
    if (direct != expected) return \"fail direct=$direct\"\n\
    return \"OK\"\n\
}\n\
\n\
fun box(): String = bar<Int>(typeOf<List<Int>>())\n\
";

fn reflect_jar() -> std::path::PathBuf {
    common::dist_jar("kotlin-reflect.jar")
        .or_else(|| common::find_jar("kotlin-reflect-", &["sources"]))
        .expect("kotlin-reflect.jar from the provisioned kotlinc distribution")
}

#[test]
fn reified_type_of_in_an_anonymous_property_matches_kotlinc() {
    let reflect = reflect_jar();
    let reference =
        common::kotlinc_box_result_with_classpath(SOURCE, std::slice::from_ref(&reflect));
    assert_eq!(reference, "OK", "kotlinc");
    assert_eq!(
        common::Fixture::new().with_reflect().run_box(SOURCE),
        reference,
        "krusty"
    );
}
