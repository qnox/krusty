//! `+DeprecateNameMismatchInShortDestructuringWithParentheses`: kotlinc warns for each positional
//! entry of the parenthesized short destructuring form (`val (a, b)`, `for ((a, b) in …)`,
//! `{ (a, b) -> }`) whose meaning the planned name-based reading would break. Each fixture is
//! compiled by both compilers; the complete warning blocks (location, every message line, count and
//! order) must be the expected ones for kotlinc and identical for krusty.

use super::common;

const FEATURE: &str = "-XXLanguage:+DeprecateNameMismatchInShortDestructuringWithParentheses";
const SEE_MORE: &str = "| See https://kotl.in/name-based-destructuring for more information.";

fn mismatch(at: &str, entry: &str, property: &str) -> Vec<String> {
    vec![
        format!(
            "main.kt:{at}: variable name '{entry}' differs from accessed property name '{property}'. \
             This syntax will be used for name-based destructuring in a future release, and this \
             code will change its meaning."
        ),
        format!(
            "| Use the full name-based destructuring syntax '(val {entry} = {property}, ...)', the \
             new positional destructuring syntax '[{entry}, ...]', or align the names to prepare \
             for the transition."
        ),
        SEE_MORE.to_string(),
    ]
}

fn non_data(at: &str, declaration: &str, ty: &str, entry: &str) -> Vec<String> {
    vec![
        format!(
            "main.kt:{at}: this syntax will be used for name-based destructuring in a future \
             release which will stop compiling or change its meaning for {declaration} '{ty}'."
        ),
        format!(
            "| Use the new positional destructuring syntax '[{entry}, ...]' to prepare for the \
             transition."
        ),
        SEE_MORE.to_string(),
    ]
}

fn underscore(at: &str) -> Vec<String> {
    vec![
        format!(
            "main.kt:{at}: this syntax will be used for name-based destructuring in a future \
             release, and an underscore without renaming will become an error."
        ),
        "| Use name-based destructuring syntax '(val x, ...)' and drop the unused entry, or use \
         the new positional destructuring syntax '[_, ...]'."
            .to_string(),
        SEE_MORE.to_string(),
    ]
}

/// Compile `source` with both compilers, kotlinc receiving `reference_args` (krusty reads the
/// fixture's own `// LANGUAGE:` directive), require both to accept it, and require both to report
/// exactly `expected`.
fn assert_warnings(source: &str, reference_args: &[&str], expected: &[Vec<String>]) {
    let reference_args = reference_args
        .iter()
        .map(|argument| argument.to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_reference_args(
        &[("main.kt", source)],
        &[],
        &reference_args,
    );
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    let expected = expected.concat();
    assert_eq!(
        common::warning_blocks(&result.reference_stderr, true),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::warning_blocks(&result.krusty_stderr, false),
        expected
    );
}

const SHORT_FORMS: &str = r#"// LANGUAGE: +DeprecateNameMismatchInShortDestructuringWithParentheses
data class P(val first: Int, val second: Int)
data class Q(val x: Int) { operator fun component2() = 2 }
class C { operator fun component1() = 1; operator fun component2() = 2 }
class D(val a: Int) { operator fun component1() = a }
interface Base { operator fun component1(): Int = 1; operator fun component2(): Int = 2 }
class Impl : Base
typealias PA = P
fun <T : Base> generic(t: T): Int { val (g1, g2) = t; return g1 + g2 }
fun use(f: (P) -> Int) = f(P(1, 2))
fun box(): String {
    val (first, second) = P(1, 2)
    val (a: Int, _) = P(1, 2)
    val (c1, c2) = C()
    val (dd) = D(1)
    val (l1, l2) = listOf(1, 2)
    for ((k, v) in mapOf(1 to 2)) {}
    val (pf, ps) = 1 to 2
    use { (x, y) -> x + y }
    var (va, vb) = P(1, 2)
    val (x, y) = Q(1)
    for ((i, value) in listOf("a").withIndex()) {}
    val pa: PA = P(1, 2); val (aa, bb) = pa
    val (t1, t2, third) = Triple(1, 2, 3)
    val base: Base? = Impl()
    if (base != null) { val (b1, b2) = base }
    val (_, `_`) = P(1, 2)
    for ((key, value) in mutableMapOf(1 to 2)) {}
    val entry: Map.Entry<String, Int> = mapOf("k" to 1).entries.first()
    val (key, count) = entry
    return if (first + second + a + c1 + c2 + dd + l1 + l2 + pf + ps + va + vb + x + y + aa + bb + t1 + t2 + third + key.length + count + generic(Impl()) > 0) "OK" else "fail"
}
"#;

#[test]
fn positional_short_form_entries_warn_like_kotlinc() {
    assert_warnings(
        SHORT_FORMS,
        &[FEATURE],
        &[
            mismatch("13:10", "a", "first"),
            underscore("13:18"),
            non_data("14:10", "non-data class", "C", "c1"),
            non_data("15:10", "non-data class", "D", "dd"),
            non_data("16:10", "non-data class", "List<Int>", "l1"),
            mismatch("17:11", "k", "key"),
            mismatch("17:14", "v", "value"),
            mismatch("18:10", "pf", "first"),
            mismatch("18:14", "ps", "second"),
            mismatch("19:12", "x", "first"),
            mismatch("19:15", "y", "second"),
            mismatch("20:10", "va", "first"),
            mismatch("20:14", "vb", "second"),
            non_data(
                "21:13",
                "custom component operators of data class",
                "Q",
                "y",
            ),
            mismatch("22:11", "i", "index"),
            mismatch("23:32", "aa", "first"),
            mismatch("23:36", "bb", "second"),
            mismatch("24:10", "t1", "first"),
            mismatch("24:14", "t2", "second"),
            non_data("26:30", "non-data class", "Base", "b1"),
            underscore("27:10"),
            mismatch("27:13", "_", "second"),
            non_data(
                "28:11",
                "non-data class",
                "MutableMap.MutableEntry<Int, Int>",
                "key",
            ),
            mismatch("30:15", "count", "value"),
        ],
    );
}

#[test]
fn short_form_entries_do_not_warn_without_the_feature() {
    let source = SHORT_FORMS.replace(
        "// LANGUAGE: +DeprecateNameMismatchInShortDestructuringWithParentheses",
        "// No language features.",
    );
    assert_warnings(&source, &[], &[]);
}

#[test]
fn name_based_short_form_has_no_positional_entries_to_warn_about() {
    const SOURCE: &str =
        "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm, \
        +DeprecateNameMismatchInShortDestructuringWithParentheses
data class Q(val first: Int) {
    val second: Int get() = 2
    operator fun component2() = second
}
fun box(): String {
    val (first, second) = Q(1)
    for ((first, second) in listOf(1 to 2)) {}
    return if (first + second == 3) \"OK\" else \"fail\"
}
";
    assert_warnings(
        SOURCE,
        &[
            "-XXLanguage:+NameBasedDestructuring",
            "-XXLanguage:+EnableNameBasedDestructuringShortForm",
            FEATURE,
        ],
        &[],
    );
    // Positional, the same declaration warns: `second` reads a custom component operator.
    let positional = SOURCE.replace(
        "+NameBasedDestructuring, +EnableNameBasedDestructuringShortForm, ",
        "",
    );
    assert_warnings(
        &positional,
        &[FEATURE],
        &[non_data(
            "7:17",
            "custom component operators of data class",
            "Q",
            "second",
        )],
    );
}

#[test]
fn full_and_bracket_forms_do_not_warn() {
    const SOURCE: &str = "// LANGUAGE: +NameBasedDestructuring, \
        +DeprecateNameMismatchInShortDestructuringWithParentheses
data class P(val first: Int, val second: Int)
class C { operator fun component1() = 1; operator fun component2() = 2 }
fun use(f: (P) -> Int) = f(P(1, 2))
fun box(): String {
    (val first, val second) = P(1, 2)
    val [a, b] = C()
    [val c, val d] = P(1, 2)
    for ([k, v] in mapOf(1 to 2)) {}
    for ((val key, val value) in mapOf(1 to 2)) {}
    val sum = use { [x, y] -> x + y } + use { (val first, val second) -> first + second }
    return if (first + second + a + b + c + d + sum > 0) \"OK\" else \"fail\"
}
";
    assert_warnings(
        SOURCE,
        &["-XXLanguage:+NameBasedDestructuring", FEATURE],
        &[],
    );
}
