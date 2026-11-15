//! A property reference used as a function value is a callable reference like a function
//! reference. Passed to an inline function's parameter it is spliced as the property read, as
//! kotlinc inlines it; anywhere else it is the property reference itself, kotlinc's
//! `PropertyReference1Impl` carrier.

use super::common;

const INLINED: &str = "\
class Point(val x: Int)\n\
\n\
private val Point.label: String\n\
    get() = \"p$x\"\n\
\n\
inline fun <T, R> T.twice(transform: (T) -> R): R {\n\
    transform(this)\n\
    return transform(this)\n\
}\n\
\n\
fun labels(points: List<Point>): List<String> = points.map(Point::label)\n\
\n\
fun coordinates(points: List<Point>): List<Int> = points.map(Point::x)\n\
\n\
fun moduleInline(point: Point): Int = point.twice(Point::x)\n\
\n\
fun bound(point: Point): Int = run(point::x)\n\
";

/// The dependency `map`, a same-module inline function, and a bound reference each splice the
/// getter call where kotlinc does, and no adapter method is left on the facade.
#[test]
fn a_property_reference_passed_to_an_inline_function_is_inlined() {
    let differences = common::class_bytes_diffs_against_kotlinc(
        "InlinedPropertyReference",
        &[],
        INLINED,
        "InlinedPropertyReferenceKt",
        &[
            "public static final java.util.List<java.lang.String> labels(",
            "public static final java.util.List<java.lang.Integer> coordinates(",
            "public static final int moduleInline(",
            "public static final int bound(",
        ],
    )
    .expect("reference kotlinc is provisioned");
    for difference in differences {
        assert_eq!(difference, Ok(()));
    }
}

const VALUE: &str = "\
class Point(val x: Int)\n\
\n\
fun apply(transform: (Point) -> Int): Int = transform(Point(1))\n\
\n\
fun unbound(): Int = apply(Point::x)\n\
";

/// A property reference passed as an ordinary function value is the property reference object.
#[test]
fn a_property_reference_function_value_is_a_property_reference_carrier() {
    for (class, method) in [
        (
            "PropertyReferenceValueKt",
            "public static final int unbound(",
        ),
        (
            "PropertyReferenceValueKt$unbound$1",
            "public java.lang.Object get(java.lang.Object);",
        ),
    ] {
        let difference = common::class_bytes_diff_against_kotlinc(
            "PropertyReferenceValue",
            &[],
            VALUE,
            class,
            method,
        )
        .expect("reference kotlinc is provisioned");
        assert_eq!(difference, Ok(()), "{class}");
    }
}

const EQUALITY: &str = "\
class Point(val x: Int)\n\
\n\
fun value(transform: (Point) -> Int): Any = transform\n\
\n\
fun box(): String {\n\
    val first = value(Point::x)\n\
    val second = value(Point::x)\n\
    if (first != second) return \"unequal\"\n\
    return if (first.hashCode() == second.hashCode()) \"OK\" else \"hash\"\n\
}\n\
";

/// Two function values of the same property reference are equal, as property references are.
#[test]
fn property_reference_function_values_are_equal() {
    common::expect_box_same_as_kotlinc(EQUALITY, "PropertyReferenceEquality");
}
