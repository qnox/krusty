//! A reified `enumEntries<T>()` read inside a lambda that an inline function returns.
//!
//! The lambda is an anonymous object copied into the calling class, and its reified marker is
//! specialized there. The read must name the enum's entries the way the call site does, through the
//! enum's published accessor or the calling class's mapping class. The copy refused every enum
//! ("the compiled inline body of `mk` cannot be inlined"), and from a property initializer it
//! panicked while computing frames.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const IN_FUNCTION: &str = r#"import kotlin.enums.EnumEntries
import kotlin.enums.enumEntries

inline fun <reified T : Enum<T>> mk(): () -> EnumEntries<T> = { enumEntries<T>() }

enum class Ver { A, B }

fun box(): String {
    val entries = mk<Ver>()()
    return if (entries.toList() == listOf(Ver.A, Ver.B)) "OK" else "FAIL: $entries"
}
"#;

const IN_INITIALIZER: &str = r#"import kotlin.enums.EnumEntries
import kotlin.enums.enumEntries

inline fun <reified T : Enum<T>> contains(): (String) -> Boolean =
    { name: String -> enumEntries<T>().any { it.name == name } }

enum class Ver(val releaseName: String) { A("a"), B("b") }

class Holder { val has = contains<Ver>() }

fun box(): String = if (Holder().has("B") && !Holder().has("C")) "OK" else "FAIL"
"#;

#[test]
fn a_reified_entries_read_in_a_copied_lambda_matches_kotlinc() {
    assert_like_kotlinc(&[("InFunction.kt", IN_FUNCTION)]);
}

#[test]
fn a_reified_entries_read_in_a_property_initializer_matches_kotlinc() {
    assert_like_kotlinc(&[("InInitializer.kt", IN_INITIALIZER)]);
}

#[test]
fn a_reified_entries_read_in_a_copied_lambda_runs() {
    for (source, stem) in [
        (IN_FUNCTION, "InFunction"),
        (IN_INITIALIZER, "InInitializer"),
    ] {
        let outcome = common::compile_and_run_with_stdlib(source, stem);
        assert_eq!(outcome.as_deref(), Some("OK"), "{stem}");
    }
}
