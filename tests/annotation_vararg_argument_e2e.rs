//! A whole array passed to an annotation's vararg parameter, spread or named, is checked against
//! the parameter's type `Array<out E>`, for a Kotlin and a Java annotation alike. kotlinc reads an
//! `arrayOf`, `emptyArray` or primitive `…ArrayOf` call in an annotation argument as an array
//! literal, so the checks of a call's reified type arguments and of an `Array<Nothing>` call
//! result do not apply to it; any other call there is still checked.

use super::common;

#[test]
fn a_spread_array_of_mixed_elements_is_no_string_vararg() {
    let source = "\
annotation class B(vararg val args: String)
@B(*arrayOf(1, \"b\"))
fun spread() {}
@B(args = arrayOf(1))
fun named() {}
@B(*arrayOf(\"a\"), *arrayOf(2))
fun second() {}
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:2:5: argument type mismatch: actual type is 'Array<Comparable<*> & Serializable>', but 'Array<out String>' was expected.",
            "test.kt:4:11: argument type mismatch: actual type is 'Array<Int>', but 'Array<out String>' was expected.",
            "test.kt:6:20: argument type mismatch: actual type is 'Array<Int>', but 'Array<out String>' was expected.",
        ]
    );
}

/// Outside an annotation the same join is an ordinary inferred intersection, reified by the call.
#[test]
fn a_mixed_array_of_call_infers_its_elements_common_supertypes() {
    let source = "\
val y: Array<String> = arrayOf(1, \"b\")
fun k(a: Array<String>) {}
fun m() { k(arrayOf(1, \"b\")) }
";
    common::assert_error_blocks_match_kotlinc(&[("test.kt", source)], &[]);
}

#[test]
fn a_java_annotations_value_array_is_its_vararg_and_its_other_arrays_are_invariant() {
    let java = "\
public @interface A {
    String[] value();
    String[] names() default {};
}
";
    let source = "\
@A(*arrayOf(1, \"b\"))
fun spread() {}
@A(value = arrayOf(1))
fun named() {}
@A(\"a\", names = arrayOf(1))
fun other() {}
";
    let sources = [("A.java", java), ("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:1:5: argument type mismatch: actual type is 'Array<Comparable<*> & Serializable>', but 'Array<out String>' was expected.",
            "test.kt:3:12: argument type mismatch: actual type is 'Array<Int>', but 'Array<out String>' was expected.",
            "test.kt:5:17: argument type mismatch: actual type is 'Array<Int>', but 'Array<String>' was expected.",
        ]
    );
}

#[test]
fn an_array_factory_in_an_annotation_argument_is_a_literal() {
    let source = "\
annotation class C(val args: Array<String>)
annotation class E(val s: String)
@C(arrayOf<Nothing>())
fun nothing() {}
@C(emptyArray<Nothing>())
fun empty() {}
@C(arrayOf(1, \"b\"))
fun mixed() {}
inline fun <reified T> name(): String = \"\"
@E(name<Nothing>())
fun call() {}
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:3:4: argument type mismatch: actual type is 'Array<Nothing>', but 'Array<String>' was expected.",
            "test.kt:5:4: argument type mismatch: actual type is 'Array<Nothing>', but 'Array<String>' was expected.",
            "test.kt:7:4: argument type mismatch: actual type is 'Array<Comparable<*> & Serializable>', but 'Array<String>' was expected.",
            "test.kt:10:4: annotation argument must be a compile-time constant.",
            "test.kt:10:9: cannot use 'Nothing' as reified type parameter.",
        ]
    );
}

#[test]
fn string_arrays_fill_a_string_vararg() {
    let java = "public @interface A { String[] value(); }\n";
    let source = "\
annotation class B(vararg val args: String)
@B(*arrayOf(\"a\", \"b\"), \"c\")
@A(*arrayOf(\"a\"), *arrayOf(\"b\"))
fun spread() {}
@B(args = arrayOf(\"a\"))
@A(value = [\"a\"])
fun named() {}
";
    common::assert_sources_accepted_like_kotlinc(&[("A.java", java), ("test.kt", source)], &[]);
}

/// An expected `Array<out E>` only bounds an array factory's element: the array keeps the
/// argument's own element type, whether the expectation is a projection, an inferred spread, or a
/// plain literal.
#[test]
fn a_projected_array_expectation_keeps_the_arguments_element_type() {
    let source = "\
fun box(): String {
    val projected: Array<out CharSequence> = arrayOf(*arrayOf(\"x\"))
    val inferred = arrayOf(*arrayOf(\"x\"))
    val literal: Array<out CharSequence> = arrayOf(\"x\")
    val names = projected.javaClass.componentType.name + inferred.javaClass.componentType.name +
        literal.javaClass.componentType.name
    return if (names == \"java.lang.Stringjava.lang.Stringjava.lang.String\") \"OK\" else \"fail $names\"
}
";
    common::expect_box_same_as_kotlinc(source, "projected_array_expectation");
}
