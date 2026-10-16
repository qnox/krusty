//! A nested generic producer is one more lower bound for an outer variable, not an equation.
//!
//! In `same(list, emptyList())` the first argument already gives `T` the lower bound
//! `MutableList<String>`. The producer's `List<E>` cannot equal that, but it reaches `T` through the
//! `List` supertype of the existing evidence, so `E = String` and `T` becomes the common supertype
//! `List<String>`. When the expected result also bounds the variable from above, the producer is
//! solved under that bound: `listOf(real, Unimpl())` expected as `List<Provider<Builder>>` gives
//! `Unimpl<Builder>`, and the element type joins to `Provider<Builder>`.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

const DECLARATIONS: &str = r#"
fun <T> same(actual: T, expected: T): T = actual
fun <T> sameOrNull(actual: T, expected: T?): T = actual
infix fun <T> T.shouldBe(expected: T?): T = this
"#;

#[test]
fn a_nested_producer_joins_the_existing_lower_bound() {
    let source = format!(
        "{DECLARATIONS}{}",
        r#"
class Holder { val values: MutableList<String>? = mutableListOf() }

fun box(): String {
    val list: MutableList<String> = mutableListOf()
    val joined: List<String> = same(list, emptyList())
    if (joined !== list) return "same"
    if (sameOrNull(list, emptyList()) !== list) return "sameOrNull"
    if (Holder().values shouldBe emptyList() != emptyList<String>()) return "nullable receiver"
    if ((list shouldBe listOf()) !== list) return "infix"
    return "OK"
}
"#
    );
    common::expect_box_same_as_kotlinc(&source, "NestedProducerLowerBound");
}

#[test]
fn a_vararg_producer_is_solved_under_the_expected_upper_bound() {
    common::expect_box_same_as_kotlinc(
        r#"
interface Provider<T> { fun probe(): T? }
class Unimpl<T>(val reason: String) : Provider<T> { override fun probe(): T? = null }
interface Builder
object Real : Provider<Builder> { override fun probe(): Builder = object : Builder {} }

val providers: List<Provider<Builder>> = listOf(Unimpl("first"), Real, Unimpl("last"))
val trailing: List<Provider<Builder>> = listOf(Real, Unimpl("only"))

fun box(): String {
    val found = providers.mapNotNull { it.probe() } + trailing.mapNotNull { it.probe() }
    return if (found.size == 2) "OK" else "found ${found.size}"
}
"#,
        "VarargProducerUpperBound",
    );
}

#[test]
fn the_joined_variable_is_not_the_narrower_bound() {
    let source = "fun <T> same(actual: T, expected: T): T = actual\n\
                  \n\
                  fun probe(list: MutableList<String>) {\n    \
                      val joined = same(list, emptyList())\n    \
                      val narrow: MutableList<String> = joined\n\
                  }\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let expected = vec![ObservedError {
        file: "Use.kt".to_string(),
        line: 5,
        column: 37,
        message:
            "initializer type mismatch: expected 'MutableList<String>', actual 'List<String>'."
                .to_string(),
    }];
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}
