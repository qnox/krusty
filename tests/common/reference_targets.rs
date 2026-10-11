//! The reference compilers as oracles: kotlinc, kotlinc-js, kotlinc-wasm and kotlinc-native.
//!
//! A test asks one what it does with a source set on its target: whether it accepts it, and every
//! error it reports, located and complete. kotlinc-native also answers what a program's `box()`
//! returns. The answer is recorded in the shared reference archive under the exact compiler
//! version (see `byte_dump`), so a later run replays it and the reference compiler runs only for a
//! source set it has not seen.
//!
//! kotlinc-js and kotlinc-wasm ship in the kotlinc distribution every test run provisions.
//! kotlinc-native is its own distribution (`just kotlin-native`), named by `KRUSTY_KOTLIN_NATIVE`.
//! Without it a recorded answer still replays; an unrecorded one is unavailable, which the
//! `klib-semantics` lane turns into a failure with `KRUSTY_REQUIRE_KOTLIN_NATIVE`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::compilation_target::CompilationTarget;

use super::common::byte_dump::{self, fingerprint_parts};

/// What a compiler did with a source set: whether it accepted it, and every error it reported in
/// emission order. Each error is one complete block, `file:line:column: message`, with any further
/// message lines after a newline as `| line`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerOutcome {
    pub accepted: bool,
    pub errors: Vec<String>,
}

impl CompilerOutcome {
    /// A reference compiler's outcome from its exit status and stderr. A rejection must name its
    /// errors and an acceptance must have none: anything else is a failed or misconfigured
    /// invocation, which is never an answer to record.
    fn from_run(status: i32, stderr: &str) -> Self {
        let mut errors: Vec<String> = Vec::new();
        for line in super::common::reference_error_blocks_from(stderr) {
            match errors.last_mut() {
                Some(block) if line.starts_with("| ") => {
                    block.push('\n');
                    block.push_str(&line);
                }
                _ => errors.push(line),
            }
        }
        let accepted = status == 0;
        assert!(
            accepted == errors.is_empty(),
            "the reference compiler exited with {status} and reported {} error(s):\n{stderr}",
            errors.len()
        );
        Self { accepted, errors }
    }

    /// Length-delimited: an error block may span lines, and is recorded as one entry.
    fn encode(&self) -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::from([("accepted".to_string(), vec![u8::from(self.accepted)])]);
        for (index, error) in self.errors.iter().enumerate() {
            files.insert(format!("error-{index:04}"), error.clone().into_bytes());
        }
        files
    }

    fn decode(files: &BTreeMap<String, Vec<u8>>) -> Self {
        let accepted = match files.get("accepted").map(Vec::as_slice) {
            Some([0]) => false,
            Some([1]) => true,
            other => panic!("a reference recording with no acceptance: {other:?}"),
        };
        let errors = files
            .iter()
            .filter(|(name, _)| name.starts_with("error-"))
            .map(|(_, bytes)| String::from_utf8(bytes.clone()).expect("recorded text is UTF-8"))
            .collect();
        Self { accepted, errors }
    }
}

/// What the reference compiler for `target` does with `sources`. `None` when kotlinc-native is not
/// provisioned and no recording exists; the kotlinc distribution is always provisioned.
pub fn reference_outcome(
    target: CompilationTarget,
    sources: &[(&str, &str)],
    args: &[String],
) -> Option<CompilerOutcome> {
    let files = recorded(target, "outcome", sources, args, |work| {
        let outcome = if target == CompilationTarget::Jvm {
            let (status, stderr) = super::common::reference_compiler_run(sources, args);
            CompilerOutcome::from_run(status, &stderr)
        } else {
            let paths = super::common::write_fixture_sources(work, sources);
            let run = run_reference(target, work, &paths, args, Output::Library)?;
            CompilerOutcome::from_run(run.status, &run.stderr)
        };
        Some(outcome.encode())
    })?;
    Some(CompilerOutcome::decode(&files))
}

/// What `box()` answers when kotlinc-native builds and runs `source`. `None` when kotlin-native is
/// not provisioned and no recording exists.
pub fn kotlin_native_box(source: &str) -> Option<String> {
    let program = format!("{source}\nfun main() {{ print(box()) }}\n");
    let sources = [("box.kt", program.as_str())];
    let files = recorded(
        CompilationTarget::Native,
        "program",
        &sources,
        &[],
        |work| {
            let paths = super::common::write_fixture_sources(work, &sources);
            let built = run_reference(
                CompilationTarget::Native,
                work,
                &paths,
                &[],
                Output::Program,
            )?;
            assert!(
                built.status == 0,
                "kotlinc-native rejected a box program:\n{}",
                built.stderr
            );
            let run = Command::new(work.join("program.kexe"))
                .output()
                .expect("run the kotlinc-native program");
            assert!(
                run.status.success(),
                "the kotlinc-native program exited with {}:\n{}",
                run.status,
                String::from_utf8_lossy(&run.stderr)
            );
            Some(BTreeMap::from([("stdout".to_string(), run.stdout)]))
        },
    )?;
    let stdout = files
        .get("stdout")
        .expect("a box recording holds the program's stdout");
    Some(String::from_utf8(stdout.clone()).expect("recorded text is UTF-8"))
}

/// Read the recording for this source set, or produce and record it with `live`.
fn recorded(
    target: CompilationTarget,
    kind: &str,
    sources: &[(&str, &str)],
    args: &[String],
    live: impl FnOnce(&Path) -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<BTreeMap<String, Vec<u8>>> {
    let fingerprint = fingerprint(target, kind, sources, args);
    let slot = format!("reference-{kind}-{target:?}-{fingerprint:032x}");
    if let Some(files) = byte_dump::load_shared_files(&slot, fingerprint) {
        return Some(files);
    }
    let work = super::common::scratch_dir().expect("cannot allocate a reference-compiler fixture");
    let answer = live(&work);
    let _ = std::fs::remove_dir_all(&work);
    let answer = answer?;
    byte_dump::store_shared_files(&slot, fingerprint, &answer);
    Some(answer)
}

fn fingerprint(
    target: CompilationTarget,
    kind: &str,
    sources: &[(&str, &str)],
    args: &[String],
) -> u128 {
    let mut blob = Vec::new();
    let mut field = |bytes: &[u8]| {
        blob.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        blob.extend_from_slice(bytes);
    };
    field(format!("{target:?}").as_bytes());
    field(kind.as_bytes());
    for (name, text) in sources {
        field(name.as_bytes());
        field(text.as_bytes());
    }
    for argument in args {
        field(argument.as_bytes());
    }
    fingerprint_parts(&[&blob])
}

enum Output {
    /// Analyze and serialize a KLIB: every frontend diagnostic, no native code generation.
    Library,
    /// Build an executable `program.kexe` in the work directory.
    Program,
}

struct Reference {
    status: i32,
    stderr: String,
}

/// Run the non-JVM reference compiler for `target`. `None` when it is not provisioned.
fn run_reference(
    target: CompilationTarget,
    work: &Path,
    paths: &[PathBuf],
    args: &[String],
    output: Output,
) -> Option<Reference> {
    let out = work.join("reference-out");
    let mut command = match target {
        CompilationTarget::Jvm => {
            panic!("the JVM reference compiler runs through `reference_compiler_run`")
        }
        CompilationTarget::Native => {
            let mut command = Command::new(kotlin_native_root()?.join("bin/kotlinc-native"));
            match output {
                Output::Library => command.args(["-p", "library", "-o"]).arg(&out),
                Output::Program => command
                    .args(["-p", "program", "-o"])
                    .arg(work.join("program")),
            };
            command
        }
        CompilationTarget::Js | CompilationTarget::WasmJs | CompilationTarget::WasmWasi => {
            assert!(
                matches!(output, Output::Library),
                "only Kotlin/Native programs are run by this oracle"
            );
            let lib = krusty::toolchain::kotlinc_lib_dir()
                .expect("the kotlinc distribution is provisioned for every test run");
            let bin = lib.with_file_name("bin");
            let (compiler, stdlib, flags): (_, _, &[&str]) = match target {
                CompilationTarget::Js => ("kotlinc-js", "kotlin-stdlib-js.klib", &[]),
                CompilationTarget::WasmJs => (
                    "kotlinc-wasm",
                    "kotlin-stdlib-wasm-js.klib",
                    &["-Xwasm-target=wasm-js"],
                ),
                _ => (
                    "kotlinc-wasm",
                    "kotlin-stdlib-wasm-wasi.klib",
                    &["-Xwasm-target=wasm-wasi"],
                ),
            };
            let mut command = Command::new(bin.join(compiler));
            command
                .args(flags)
                .args(["-Xir-produce-klib-dir", "-libraries"])
                .arg(lib.join(stdlib))
                .arg("-ir-output-dir")
                .arg(&out)
                .args(["-ir-output-name", "reference"]);
            command
        }
    };
    let output = command
        .args(args)
        .args(paths)
        .output()
        .expect("run the reference compiler");
    Some(Reference {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The Kotlin/Native distribution root, when provisioned.
fn kotlin_native_root() -> Option<PathBuf> {
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if root.is_none() && std::env::var_os("KRUSTY_REQUIRE_KOTLIN_NATIVE").is_some() {
        panic!("KRUSTY_REQUIRE_KOTLIN_NATIVE is set but KRUSTY_KOTLIN_NATIVE is not");
    }
    root
}

#[cfg(test)]
mod tests {
    use super::CompilerOutcome;

    #[test]
    #[should_panic(expected = "exited with 1 and reported 0 error(s)")]
    fn a_failed_invocation_without_errors_is_not_an_answer() {
        CompilerOutcome::from_run(1, "error: could not find the standard library\n");
    }

    #[test]
    #[should_panic(expected = "exited with 0 and reported 1 error(s)")]
    fn an_acceptance_with_errors_is_not_an_answer() {
        CompilerOutcome::from_run(0, "/w/V.kt:1:1: error: something\n");
    }

    #[test]
    fn a_multiline_error_round_trips_as_one_entry() {
        let outcome = CompilerOutcome::from_run(
            1,
            "/w/V.kt:1:1: error: first line\nsecond line\n/w/V.kt:2:3: error: next\n",
        );
        assert_eq!(
            outcome,
            CompilerOutcome {
                accepted: false,
                errors: vec![
                    "V.kt:1:1: first line\n| second line".to_string(),
                    "V.kt:2:3: next".to_string(),
                ],
            }
        );
        assert_eq!(CompilerOutcome::decode(&outcome.encode()), outcome);
    }
}
