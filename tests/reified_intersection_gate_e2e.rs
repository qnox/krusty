//! `ProhibitIntersectionReifiedTypeParameter`: a type argument inferred for a reified type parameter
//! that is an intersection type reifies as the common supertype. kotlinc reports it as an error
//! with the feature (the default) and as a warning naming the issue without it, and a compilation
//! without the feature runs with the common supertype reified. Each fixture is compiled by both
//! compilers; the complete located diagnostics (location, message, count and order) must be the
//! expected ones for kotlinc and identical for krusty.

use super::common;

const OFF: &str = "-XXLanguage:-ProhibitIntersectionReifiedTypeParameter";

/// The fixture after its first line, which selects the language settings.
const BODY: &str = r#"interface X
interface Y
object A : X, Y
object B : X, Y
class Inv<T>(val v: T)
fun <T> sel(a: T, b: T) = a
inline fun <reified T> T.valueType() = T::class.java.simpleName
@Suppress("TYPE_INTERSECTION_AS_REIFIED_WARNING")
fun quiet() = sel(A, B).valueType()
fun box(): String {
    val nested = sel(Inv(A), Inv(B)).v.valueType()
    val direct = sel(A, B).valueType()
    val result = "$nested,$direct,${quiet()}"
    return if (result == "Object,Object,Object") "OK" else "Fail: $result"
}
"#;

fn source(first_line: &str) -> String {
    format!("{first_line}\n{BODY}")
}

const MESSAGE: &str = "type argument for reified type parameter 'T' was inferred to the \
     intersection of ['X' & 'Y']. Reification of an intersection type results in the common \
     supertype being used. This may lead to subtle issues and an explicit type argument is \
     encouraged.";

const DEPRECATION: &str =
    " This will become an error in language version 2.3. See https://youtrack.jetbrains.com/issue/KTLC-13.";

fn at(positions: &[&str], suffix: &str) -> Vec<String> {
    positions
        .iter()
        .map(|position| format!("Main.kt:{position}: {MESSAGE}{suffix}"))
        .collect()
}

/// With the feature each inferred intersection is an error, a suppressed warning included: the
/// suppression names only the warning.
#[test]
fn an_inferred_intersection_is_an_error_with_the_feature() {
    let source = source("// default language settings");
    let sources = [("Main.kt", source.as_str())];
    let expected = at(&["10:25", "12:40", "13:28"], "");
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_blocks_with_args(&sources, &[]),
        expected
    );
}

/// Without the feature each inferred intersection outside the suppression is a warning naming the
/// deprecation, and the program reifies the common supertype `Any`.
#[test]
fn an_inferred_intersection_is_a_warning_without_the_feature() {
    let source = source(&format!(
        "// LANGUAGE: {}",
        OFF.trim_start_matches("-XXLanguage:")
    ));
    let sources = [("Main.kt", source.as_str())];
    let result =
        common::compiler_diagnostics_with_reference_args(&sources, &[], &[OFF.to_string()]);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    let expected = at(&["12:40", "13:28"], DEPRECATION);
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
    common::expect_box_same_as_kotlinc(&source, "ReifiedIntersection");
}
