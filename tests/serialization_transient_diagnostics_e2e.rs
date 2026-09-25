//! Exact diagnostic parity for a `@kotlinx.serialization.Transient` property without an
//! initializer.
//!
//! A transient property is not a serial element, so deserialization has nothing to store in it but
//! its initializer. kotlinc's serialization plugin rejects one that has none in its FIR checker
//! (`TRANSIENT_MISSING_INITIALIZER`), at the whole declaration, modifiers and annotations included.
//! krusty let such a class through to the backend, which declined the file with a generic
//! unsupported-construct error at `1:1` instead.
//!
//! The rule is the plugin's: without `-Xplugin` the same source compiles under both compilers.

use std::path::PathBuf;

use super::common;

/// A use of the rejected class that comes first, so the declaring file is not the first one the
/// streaming frontend processes.
const USE: &str = "fun describe(record: Record): Int = record.id\n";

/// Every shape kotlinc rejects, beside the ones it accepts: a `lateinit` or initialized transient
/// property, and an object's (it has no generated `$serializer`).
const MODEL: &str = "import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient

typealias Skip = kotlinx.serialization.Transient

@Serializable
data class Record(val id: Int, @Transient val cache: Int)

@Serializable
class Deferred(val id: Int) {
    /** Documented. */
    private @Transient val secret: Int
    @Skip
    internal var aliased: String
    @Transient lateinit var late: String
    @Transient val initialized: Int = 1
    init {
        secret = id
        aliased = \"a\"
    }
}

@Serializable
sealed class Base {
    @Transient val tag: String
    init { tag = \"t\" }
}

@Serializable
object Singleton {
    @Transient val value: Int
    init { value = 1 }
}
";

/// An annotation class of the same simple name is not the serialization annotation.
const UNRELATED: &str = "package other

import kotlinx.serialization.Serializable

annotation class Transient

@Serializable
class Kept(val id: Int) {
    @Transient val derived: Int
    init { derived = id }
}
";

fn plugin_switch() -> String {
    let plugin = common::kotlinc_lib_dir()
        .expect("the reference kotlinc distribution is provisioned")
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "the reference serialization plugin is missing at {}",
        plugin.display()
    );
    format!("-Xplugin={}", plugin.display())
}

fn classpath() -> Vec<PathBuf> {
    vec![
        common::stdlib_jar(),
        krusty::toolchain::serialization_core_jar()
            .expect("the serialization core runtime is provisioned"),
    ]
}

#[test]
fn a_transient_property_without_an_initializer_is_rejected_like_kotlinc() {
    let sources = [
        ("Use.kt", USE),
        ("Model.kt", MODEL),
        ("Unrelated.kt", UNRELATED),
    ];
    let result =
        common::compiler_diagnostics_with_shared_args(&sources, &classpath(), &[plugin_switch()]);
    // The plugin jar comes from the reference kotlinc, so its wording is that release's: 2.4.20's
    // ends the sentence with a full stop.
    let render = |errors: Vec<common::CompilerError>| {
        errors
            .into_iter()
            .map(|error| {
                format!(
                    "{}:{}:{}: {}",
                    error.file, error.line, error.column, error.message
                )
            })
            .collect::<Vec<_>>()
    };
    let kotlinc = render(common::compiler_errors(&result.reference_stderr));
    let expected = common::recorded(|| kotlinc.clone());
    assert_ne!(result.reference_code, 0, "kotlinc accepted the fixture");
    assert_eq!(kotlinc, expected, "kotlinc: {}", result.reference_stderr);
    assert_ne!(result.krusty_code, 0, "krusty accepted the fixture");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    let krusty = render(krusty);
    assert_eq!(
        krusty, expected,
        "krusty: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}

#[test]
fn without_the_plugin_a_transient_property_needs_no_initializer() {
    let sources = [
        ("Use.kt", USE),
        ("Model.kt", MODEL),
        ("Unrelated.kt", UNRELATED),
    ];
    let result = common::compiler_diagnostics(&sources, &classpath());
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the fixture: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty rejected a kotlinc-valid fixture: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
