//! An unbounded type parameter instantiates a non-null `Any` bound as `V & Any`.
//!
//! `V` is not a subtype of `Any`, but it is a subtype of `(V & Any)?`. A nullable use of `<R : Any>`
//! therefore accepts `V` and `V?`, including a lambda `(T) -> R?`. A non-null use still rejects `V`,
//! and an explicit type argument is not rewritten into the intersection.

use super::common;

const SOURCE: &str = "\
fun <T, R : Any> Iterable<T>.firstNotNull(transform: (T) -> R?): R? {\n\
    for (element in this) {\n\
        val result = transform(element)\n\
        if (result != null) return result\n\
    }\n\
    return null\n\
}\n\
fun <K, V> lookup(rows: List<Map<K, V>>, key: K): V? = rows.firstNotNull { it[key] }\n\
fun <V> head(xs: List<V>): V? = xs.firstNotNull { it }\n\
fun <V> headNull(xs: List<V?>): V? = xs.firstNotNull { it }\n\
fun <R : Any> id(x: R?): R? = x\n\
fun <V> fromNullable(v: V?): V? = id(v)\n\
fun <V> fromValue(v: V): V? = id(v)\n\
fun <V> fromNull(): V? = id(null)\n\
fun <T, R : Any> boxOf(x: T, f: (T) -> R?): R? = f(x)\n\
fun <V> boxed(v: V): V? = boxOf(v) { it }\n\
fun box(): String {\n\
    val rows = listOf(mapOf(\"a\" to 1), mapOf(\"b\" to 2))\n\
    if (lookup(rows, \"b\") != 2) return \"lookup\"\n\
    if (lookup(rows, \"missing\") != null) return \"missing\"\n\
    if (head(listOf(\"x\", \"y\")) != \"x\") return \"head\"\n\
    if (headNull(listOf(null, \"z\")) != \"z\") return \"null-head\"\n\
    if (fromNullable<String>(null) != null) return \"from-null\"\n\
    if (fromValue(\"ok\") != \"ok\") return \"from-value\"\n\
    if (fromNull<Int>() != null) return \"id-null\"\n\
    if (boxed(\"v\") != \"v\") return \"boxed\"\n\
    return \"OK\"\n\
}\n\
";

#[test]
fn an_unbounded_type_parameter_satisfies_a_nullable_any_bound() {
    common::expect_box_same_as_kotlinc(SOURCE, "definitely-non-null-bound");
}

const REJECTED: &str = "\
fun <R : Any> nn(x: R): R? = x\n\
fun <R : CharSequence> cs(x: R?): R? = x\n\
fun <T, R : Any> Iterable<T>.k(transform: (T) -> R): R? = null\n\
fun <T, R : Any> Iterable<T>.d(transform: (T) -> R?): R? = null\n\
fun <V> a(v: V): V? = nn(v)\n\
fun <V> b(v: V): V? = cs(v)\n\
fun <V> c(xs: List<V>): V? = xs.k { it }\n\
fun <V> e(xs: List<V?>): V? = xs.k { it }\n\
fun <V> f(xs: List<String>): V? = xs.d { it }\n\
fun <V> g(v: V?): V? = nn<V>(v)\n\
";

#[test]
fn a_non_null_any_bound_still_rejects_an_unbounded_argument() {
    let result = common::compiler_diagnostics(
        &[("Rejected.kt", REJECTED)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    let reference = vec![
        error(5, 26, "argument type mismatch: actual type is 'V (of fun <V> a)', but 'Any' was expected."),
        error(6, 26, "argument type mismatch: actual type is 'V (of fun <V> b)', but 'CharSequence?' was expected."),
        error(7, 37, "return type mismatch: expected 'V (of fun <V> c) & Any', actual 'V (of fun <V> c)'."),
        error(8, 38, "return type mismatch: expected 'V (of fun <V> e) & Any', actual 'V? (of fun <V> e)'."),
        error(9, 42, "return type mismatch: expected 'V? (of fun <V> f)', actual 'String'."),
        error(10, 24, "inapplicable candidate(s): fun <R : Any> nn(x: R): R?"),
        error(10, 27, "type argument is not within its bounds: type parameter 'R (of fun <R : Any> nn)' must be subtype of 'Any', but actual: 'V (of fun <V> g)'."),
    ];
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        reference,
        "kotlinc"
    );
    // Krusty spells a nullable type parameter as `V (of fun <V> f)?` where kotlinc spells
    // `V? (of fun <V> f)`, and a `CharSequence` bound without the parameter's `?`. An explicit
    // type argument is rejected as an argument mismatch rather than a separate bounds diagnostic.
    let krusty = vec![
        error(5, 26, "argument type mismatch: actual type is 'V (of fun <V> a)', but 'Any' was expected."),
        error(6, 26, "argument type mismatch: actual type is 'V (of fun <V> b)', but 'CharSequence' was expected."),
        error(7, 37, "return type mismatch: expected 'V (of fun <V> c) & Any', actual 'V (of fun <V> c)'."),
        error(8, 38, "return type mismatch: expected 'V (of fun <V> e) & Any', actual 'V (of fun <V> e)?'."),
        error(9, 42, "return type mismatch: expected 'V (of fun <V> f)?', actual 'String'."),
        error(10, 30, "argument type mismatch: actual type is 'V (of fun <V> g)?', but 'V (of fun <V> g)' was expected."),
    ];
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        krusty,
        "krusty stderr:\n{}",
        result.krusty_stderr
    );
    assert_eq!(result.reference_code, 1);
    assert_eq!(result.krusty_code, 1);
}

fn error(line: usize, column: usize, message: &str) -> common::CompilerError {
    common::CompilerError {
        file: "Rejected.kt".to_string(),
        line,
        column,
        message: message.to_string(),
    }
}
