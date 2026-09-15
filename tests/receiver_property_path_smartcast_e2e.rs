//! A smart cast survives a path rooted at the receiver's own property.
//!
//! `if (config.path != null) { config.path.length }` — where `config` is the class's own `val`
//! property — was never narrowed. The stable-path reader declined ANY non-local root that carried
//! further segments, so the proof was not even recorded and the branch still saw a nullable value:
//!
//! ```text
//! error: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver
//! ```
//!
//! and, where the value was passed on instead of dereferenced, a candidate-applicability failure.
//!
//! The identical body with `config` as a PARAMETER always worked, which is what isolated the root's
//! binding kind rather than the path's shape. Kotlin's own rule is per-property — a `val` with the
//! default getter, not open, not delegated — and the member reader already enforces exactly that for
//! the single-segment case; a longer path now walks on from the same decision.

use super::common;

const DECLARATIONS: &str = "data class Config(val path: String?, val label: String)\n\n";

fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    let krusty = common::expect_box_run_with_stdlib(main, stem);
    assert_eq!(reference, "OK", "{stem}: reference compiler");
    assert_eq!(krusty, reference, "{stem}: runtime differential");
}

fn both_compilers_reject_unstable_path(main: &str, line: usize, reference_message: &str) {
    let result = common::compiler_diagnostics(&[("Main.kt", main)], &[]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(common::compiler_errors(&result.krusty_stdout), []);
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line,
            column: 31,
            message: "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.".to_string(),
        }]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line,
            column: 20,
            message: reference_message.to_string(),
        }]
    );
}

/// The failing shape: the narrowed value is DEREFERENCED through the receiver's own property.
#[test]
fn a_receiver_property_path_narrows_for_a_dereference() {
    let main = format!(
        "{DECLARATIONS}\
class Gen(private val config: Config) {{\n\
\x20   fun show(): String {{\n\
\x20       if (config.path != null) {{\n\
\x20           return config.path.length.toString()\n\
\x20       }}\n\
\x20       return \"none\"\n\
\x20   }}\n\
}}\n\
fun box(): String {{\n\
\x20   val present = Gen(Config(\"abcd\", \"x\")).show()\n\
\x20   val absent = Gen(Config(null, \"x\")).show()\n\
\x20   if (present != \"4\") return \"FAIL: present \" + present\n\
\x20   return if (absent == \"none\") \"OK\" else \"FAIL: absent \" + absent\n\
}}\n"
    );
    both_compilers_box(&main, "receiver_property_deref");
}

/// The second corpus spelling: the narrowed value is PASSED to a parameter that does not accept
/// null, so the failure surfaced as candidate applicability rather than as a nullable receiver.
#[test]
fn a_receiver_property_path_narrows_for_an_argument() {
    let main = format!(
        "{DECLARATIONS}\
fun widthOf(text: String): Int = text.length\n\
\n\
class Gen(private val config: Config) {{\n\
\x20   fun show(): String {{\n\
\x20       if (config.path != null) {{\n\
\x20           return widthOf(config.path).toString()\n\
\x20       }}\n\
\x20       return \"none\"\n\
\x20   }}\n\
}}\n\
fun box(): String {{\n\
\x20   val present = Gen(Config(\"abcd\", \"x\")).show()\n\
\x20   val absent = Gen(Config(null, \"x\")).show()\n\
\x20   if (present != \"4\") return \"FAIL: present \" + present\n\
\x20   return if (absent == \"none\") \"OK\" else \"FAIL: absent \" + absent\n\
}}\n"
    );
    both_compilers_box(&main, "receiver_property_argument");
}

/// The control that isolates the root's BINDING KIND: the identical body with `config` as a
/// parameter always narrowed.
#[test]
fn a_parameter_rooted_path_still_narrows() {
    let main = format!(
        "{DECLARATIONS}\
fun show(config: Config): String {{\n\
\x20   if (config.path != null) {{\n\
\x20       return config.path.length.toString()\n\
\x20   }}\n\
\x20   return \"none\"\n\
}}\n\
fun box(): String {{\n\
\x20   val present = show(Config(\"abcd\", \"x\"))\n\
\x20   val absent = show(Config(null, \"x\"))\n\
\x20   if (present != \"4\") return \"FAIL: present \" + present\n\
\x20   return if (absent == \"none\") \"OK\" else \"FAIL: absent \" + absent\n\
}}\n"
    );
    both_compilers_box(&main, "parameter_rooted_path");
}

/// A `var` in the path is NOT stable and must still be refused — the widened rule may not narrow
/// what Kotlin does not. Both compilers' verdicts are asserted.
#[test]
fn a_mutable_property_in_the_path_is_still_refused() {
    const MAIN: &str = "data class Config(var path: String?, val label: String)\n\
\n\
class Gen(private val config: Config) {\n\
\x20   fun show(): String {\n\
\x20       if (config.path != null) {\n\
\x20           return config.path.length.toString()\n\
\x20       }\n\
\x20       return \"none\"\n\
\x20   }\n\
}\n";
    both_compilers_reject_unstable_path(
        MAIN,
        6,
        "smart cast to 'String' is impossible, because 'path' is a mutable property that could be mutated concurrently.",
    );
}

/// A custom getter is not a stable read either: two calls may answer differently, so the proof from
/// the first must not narrow the second.
#[test]
fn a_custom_getter_in_the_path_is_still_refused() {
    const MAIN: &str = "class Config(private val backing: String?) {\n\
\x20   val path: String?\n\
\x20       get() = backing\n\
}\n\
\n\
class Gen(private val config: Config) {\n\
\x20   fun show(): String {\n\
\x20       if (config.path != null) {\n\
\x20           return config.path.length.toString()\n\
\x20       }\n\
\x20       return \"none\"\n\
\x20   }\n\
}\n";
    both_compilers_reject_unstable_path(
        MAIN,
        9,
        "smart cast to 'String' is impossible, because 'path' is a property that has an open or custom getter.",
    );
}
