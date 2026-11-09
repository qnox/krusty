//! What a reference compile observed when it produced no class inventory, kept typed from the
//! process status onward so a caller never decides a verdict from message text.

use std::fmt;
use std::path::Path;

/// kotlinc's `COMPILATION_ERROR` exit status. Every other nonzero status is an internal error, a
/// crash, or a driver failure, none of which says anything about the program.
const COMPILATION_ERROR: i32 = 1;

/// The cache marker of a reference rejection: the rejecting module's name on the first line, then
/// kotlinc's complete diagnostics. Earlier layouts recorded one rendered line under `FAILED`, which
/// kept neither the status nor the diagnostics, so such an entry is never read as a verdict.
const REJECTED_MARKER: &str = "REJECTED";
const LEGACY_FAILURE_MARKER: &str = "FAILED";

/// Why a reference compile produced no class inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReferenceFailure {
    /// kotlinc exited with `COMPILATION_ERROR` and reported an error at a Kotlin source position:
    /// the reference rejects the program. `diagnostics` is kotlinc's complete output.
    Rejected { module: String, diagnostics: String },
    /// kotlinc finished without a verdict: an internal error or crash status, or a compilation
    /// error with no source position. `diagnostics` is its complete output.
    KotlincNoVerdict {
        module: String,
        code: i32,
        diagnostics: String,
    },
    /// The case itself keeps the reference from a verdict: javac refuses its Java, its shape or a
    /// directive is one the oracle does not model, or its compile emits no class.
    NoVerdict(String),
    /// The harness could not observe the reference at all: a missing tool or JDK, its
    /// configuration, a scratch directory, or a transport failure. Never a property of the case.
    Harness(String),
}

impl fmt::Display for ReferenceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReferenceFailure::Rejected {
                module,
                diagnostics,
            } => {
                let error = source_error(diagnostics).unwrap_or_default();
                write!(formatter, "reference module {module}: {}", error.trim())
            }
            ReferenceFailure::KotlincNoVerdict {
                module,
                code,
                diagnostics,
            } => {
                let summary = diagnostics
                    .lines()
                    .find(|line| line.contains("error"))
                    .or_else(|| diagnostics.lines().find(|line| !line.trim().is_empty()))
                    .unwrap_or("no output");
                write!(
                    formatter,
                    "reference module {module}: exit code {code}: {}",
                    summary.trim()
                )
            }
            ReferenceFailure::NoVerdict(reason) | ReferenceFailure::Harness(reason) => {
                formatter.write_str(reason)
            }
        }
    }
}

/// The first error kotlinc anchored at a Kotlin source position.
fn source_error(diagnostics: &str) -> Option<&str> {
    diagnostics
        .lines()
        .find(|line| line.contains(".kt:") && line.contains(": error:"))
}

/// Classify one kotlinc invocation of `module`: `None` is a server that never answered.
pub(super) fn kotlinc_outcome(
    module: &str,
    outcome: Option<(i32, String)>,
) -> Result<(), ReferenceFailure> {
    let Some((code, diagnostics)) = outcome else {
        return Err(ReferenceFailure::Harness("kotlinc unavailable".to_string()));
    };
    let module = module.to_string();
    match code {
        0 => Ok(()),
        COMPILATION_ERROR if source_error(&diagnostics).is_some() => {
            Err(ReferenceFailure::Rejected {
                module,
                diagnostics,
            })
        }
        code => Err(ReferenceFailure::KotlincNoVerdict {
            module,
            code,
            diagnostics,
        }),
    }
}

/// Classify one javac invocation of `module` through the pooled runner: `None` is a runner that
/// never answered, `OK` a compile, and anything else javac's own refusal.
pub(super) fn javac_outcome(module: &str, outcome: Option<String>) -> Result<(), ReferenceFailure> {
    match outcome.as_deref() {
        None => Err(ReferenceFailure::Harness(format!(
            "reference module {module}: javac unavailable"
        ))),
        Some("OK") => Ok(()),
        Some(output) => Err(ReferenceFailure::NoVerdict(format!(
            "reference module {module}: javac rejected the Java sources\n{output}"
        ))),
    }
}

/// The cached rejection in `cache`, if one was recorded with its status and diagnostics.
pub(super) fn load_rejection(cache: &Path) -> Option<ReferenceFailure> {
    let record = std::fs::read_to_string(cache.join(REJECTED_MARKER)).ok()?;
    let (module, diagnostics) = record.split_once('\n')?;
    Some(ReferenceFailure::Rejected {
        module: module.to_string(),
        diagnostics: diagnostics.to_string(),
    })
}

/// Record `failure` in `cache` when it is a rejection; every other failure stays uncached so the
/// next run observes it again.
pub(super) fn store_rejection(cache: &Path, failure: &ReferenceFailure) {
    let ReferenceFailure::Rejected {
        module,
        diagnostics,
    } = failure
    else {
        return;
    };
    remove_legacy_failure(cache);
    if std::fs::create_dir_all(cache).is_ok() {
        let _ = std::fs::write(
            cache.join(REJECTED_MARKER),
            format!("{module}\n{diagnostics}"),
        );
    }
}

/// Drop an entry an earlier layout recorded as `FAILED`, so the result observed now can take its
/// place. No reader consults that marker, so removing it cannot race a reader.
pub(super) fn remove_legacy_failure(cache: &Path) {
    if cache.join(LEGACY_FAILURE_MARKER).is_file() {
        let _ = std::fs::remove_dir_all(cache);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANCHORED: &str =
        "/w/main/sources/kotlin/0/Case.kt:3:5: error: unresolved reference 'x'.\n";

    #[test]
    fn exit_one_with_a_source_anchored_error_is_a_rejection_with_its_complete_diagnostics() {
        let diagnostics = format!("warning: ATTENTION!\n{ANCHORED}");
        assert_eq!(
            kotlinc_outcome("main", Some((1, diagnostics.clone()))),
            Err(ReferenceFailure::Rejected {
                module: "main".to_string(),
                diagnostics,
            })
        );
    }

    #[test]
    fn exit_two_with_a_source_anchored_error_is_no_verdict() {
        let diagnostics = format!("{ANCHORED}exception: java.lang.IllegalStateException: boom\n");
        let failure = kotlinc_outcome("main", Some((2, diagnostics.clone())))
            .expect_err("an internal error is not a compile");
        assert_eq!(
            failure,
            ReferenceFailure::KotlincNoVerdict {
                module: "main".to_string(),
                code: 2,
                diagnostics,
            }
        );
        assert_eq!(
            failure.to_string(),
            "reference module main: exit code 2: /w/main/sources/kotlin/0/Case.kt:3:5: error: \
             unresolved reference 'x'."
        );
    }

    #[test]
    fn exit_one_without_a_source_position_is_no_verdict() {
        let diagnostics = "error: module java.base cannot be found in the module graph\n";
        assert_eq!(
            kotlinc_outcome("main", Some((1, diagnostics.to_string()))),
            Err(ReferenceFailure::KotlincNoVerdict {
                module: "main".to_string(),
                code: 1,
                diagnostics: diagnostics.to_string(),
            })
        );
    }

    #[test]
    fn a_missing_compiler_or_runner_is_a_harness_failure() {
        assert_eq!(
            kotlinc_outcome("main", None),
            Err(ReferenceFailure::Harness("kotlinc unavailable".to_string()))
        );
        assert_eq!(
            javac_outcome("main", None),
            Err(ReferenceFailure::Harness(
                "reference module main: javac unavailable".to_string()
            ))
        );
        assert_eq!(
            javac_outcome("main", Some("Main.java:1: error: ';' expected".to_string())),
            Err(ReferenceFailure::NoVerdict(
                "reference module main: javac rejected the Java sources\n\
                 Main.java:1: error: ';' expected"
                    .to_string()
            ))
        );
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krusty_reference_verdict_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn only_a_rejection_is_cached_and_it_reads_back_whole() {
        let cache = scratch("store");
        let no_verdict = kotlinc_outcome("main", Some((2, ANCHORED.to_string()))).unwrap_err();
        store_rejection(&cache, &no_verdict);
        assert!(!cache.exists(), "an exit-2 observation must not be cached");

        let rejected = kotlinc_outcome("main", Some((1, ANCHORED.to_string()))).unwrap_err();
        store_rejection(&cache, &rejected);
        assert_eq!(load_rejection(&cache), Some(rejected));
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_legacy_text_only_failure_entry_is_not_a_verdict() {
        let cache = scratch("legacy");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join(LEGACY_FAILURE_MARKER),
            "reference module main: /w/Case.kt:1:1: error: x",
        )
        .unwrap();
        assert_eq!(load_rejection(&cache), None);
        remove_legacy_failure(&cache);
        assert!(!cache.exists());
    }
}
