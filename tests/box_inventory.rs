//! One box inventory: upstream cases and repository-owned portable regressions.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::conformance::TestTarget;

fn regressions() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_regressions")
}

pub fn owned(file: &Path) -> bool {
    file.starts_with(regressions())
}

/// Locate the pinned Kotlin repository containing the canonical codegen/box directory.
pub(super) fn checkout(corpus: &Path) -> Result<&Path, String> {
    if !corpus.ends_with("compiler/testData/codegen/box") {
        return Err(format!(
            "not an upstream codegen/box checkout: {}",
            corpus.display()
        ));
    }
    corpus
        .ancestors()
        .nth(4)
        .ok_or_else(|| "missing Kotlin checkout root".into())
}

/// The upstream cases the pinned Kotlin test generators select for `target`: kotlinc's own
/// definition of which box cases a backend runs, read from the generator sources of the same
/// checkout (`scripts/box_selection.py`).
pub(super) fn model_files(corpus: &Path, target: TestTarget) -> Result<BTreeSet<PathBuf>, String> {
    let target = match target {
        TestTarget::Jvm => "jvm",
        TestTarget::Native => "native",
        TestTarget::WasmJs => "wasm-js",
        TestTarget::WasmWasi => "wasm-wasi",
    };
    let checkout = checkout(corpus)?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/box_selection.py");
    let output = Command::new("python3")
        .arg(script)
        .arg("--checkout")
        .arg(checkout)
        .args(["--target", target])
        .output()
        .map_err(|error| format!("run upstream box selector: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "upstream box selection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    text.lines()
        .map(|line| {
            let relative = decode_path(line)?;
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(format!("invalid upstream selection path: {relative:?}"));
            }
            let path = checkout.join(relative);
            if !path.is_file() {
                return Err(format!(
                    "selected upstream case is missing: {}",
                    path.display()
                ));
            }
            Ok(path)
        })
        .collect()
}

fn decode_path(encoded: &str) -> Result<PathBuf, String> {
    if encoded.is_empty() || !encoded.is_ascii() || !encoded.len().is_multiple_of(2) {
        return Err("invalid encoded upstream selection path".into());
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&encoded[index..index + 2], 16).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PathBuf::from(
        String::from_utf8(bytes).map_err(|error| error.to_string())?,
    ))
}

pub fn key(corpus: &Path, file: &Path) -> String {
    let regressions = regressions();
    let relative = file
        .strip_prefix(&regressions)
        .map(|path| Path::new("regressions").join(path))
        .unwrap_or_else(|_| file.strip_prefix(corpus).unwrap_or(file).to_path_buf());
    relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn join(
    corpus: &Path,
    upstream: Vec<PathBuf>,
    owned: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    let mut keys = BTreeSet::new();
    for file in &upstream {
        let key = key(corpus, file);
        if key.starts_with("regressions/multimodule/") {
            return Err(format!(
                "upstream box key uses reserved regressions namespace: {key}"
            ));
        }
    }
    let mut files = upstream;
    files.extend(owned);
    files.sort_by_key(|file| key(corpus, file));
    for file in &files {
        let key = key(corpus, file);
        if !keys.insert(key.clone()) {
            return Err(format!("duplicate box key {key}"));
        }
    }
    Ok(files)
}

/// The cases a `target` lane runs: the codegen/box cases that target's pinned generators select,
/// and every owned regression (whose own directives decide its applicability).
pub fn discover(corpus: &Path, target: TestTarget) -> Result<Vec<PathBuf>, String> {
    if !corpus.is_dir() {
        return Err(format!("box corpus is unavailable: {}", corpus.display()));
    }
    let regressions = regressions();
    if !regressions.is_dir() {
        return Err(format!(
            "owned box regressions are unavailable: {}",
            regressions.display()
        ));
    }
    let selected = model_files(corpus, target)?;
    let upstream: Vec<PathBuf> = krusty::conformance::kotlin_files(corpus)
        .into_iter()
        .filter(|file| selected.contains(file))
        .collect();
    let owned = krusty::conformance::kotlin_files(&regressions);
    if upstream.is_empty() || owned.is_empty() {
        return Err("box inventory requires both upstream sources and owned regressions".into());
    }
    join(corpus, upstream, owned)
}

pub fn require_setup(unavailable: Option<&str>, provisioned: bool) -> Result<(), String> {
    if let Some(reason) = unavailable {
        return Err(format!("box lane is unavailable: {reason}"));
    }
    if !provisioned {
        return Err("box sweep requires the provisioned upstream corpus".into());
    }
    Ok(())
}

#[test]
fn upstream_and_owned_cases_share_stable_unique_keys() {
    let corpus = Path::new("/corpus");
    let upstream = corpus.join("inline/a.kt");
    let first = regressions().join("multimodule/a.kt");
    let later = regressions().join("multimodule/later.kt");
    let files = join(
        corpus,
        vec![upstream.clone()],
        vec![later.clone(), first.clone()],
    )
    .unwrap();
    assert_eq!(
        files
            .iter()
            .map(|file| key(corpus, file))
            .collect::<Vec<_>>(),
        vec![
            "inline/a.kt",
            "regressions/multimodule/a.kt",
            "regressions/multimodule/later.kt"
        ]
    );
    assert!(!owned(&upstream));
    assert!(owned(&first));
}

#[test]
fn duplicate_keys_and_reserved_upstream_namespace_are_rejected() {
    let corpus = Path::new("/corpus");
    let file = corpus.join("a.kt");
    assert_eq!(
        join(corpus, vec![file.clone(), file], vec![]),
        Err("duplicate box key a.kt".into())
    );
    let reserved = corpus.join("regressions/multimodule/a.kt");
    assert_eq!(
        join(corpus, vec![reserved], vec![]),
        Err(
            "upstream box key uses reserved regressions namespace: regressions/multimodule/a.kt"
                .into()
        )
    );
}

#[test]
fn a_sweep_cannot_succeed_without_its_configured_lane_or_upstream_corpus() {
    assert_eq!(
        require_setup(Some("missing compiler"), true),
        Err("box lane is unavailable: missing compiler".into())
    );
    assert_eq!(
        require_setup(None, false),
        Err("box sweep requires the provisioned upstream corpus".into())
    );
    assert_eq!(require_setup(None, true), Ok(()));
}

#[test]
fn missing_corpus_is_not_replaced_by_owned_cases() {
    let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/no-such-box-corpus");
    assert_eq!(
        discover(&missing, TestTarget::Jvm),
        Err(format!("box corpus is unavailable: {}", missing.display()))
    );
}

/// kotlinc's Wasm generators exclude the JVM-only `compileKotlinAgainstKotlin` directory, which the
/// JVM generator selects; an owned regression runs on every lane.
#[test]
fn each_lane_runs_the_cases_its_pinned_generators_select() {
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    let jvm = discover(&corpus, TestTarget::Jvm).unwrap();
    let wasm = discover(&corpus, TestTarget::WasmJs).unwrap();
    let separate = |files: &[PathBuf]| {
        files
            .iter()
            .any(|file| key(&corpus, file).starts_with("compileKotlinAgainstKotlin/"))
    };
    assert!(separate(&jvm));
    assert!(!separate(&wasm));
    let owned_cases = |files: &[PathBuf]| files.iter().filter(|file| owned(file)).count();
    assert_eq!(owned_cases(&jvm), owned_cases(&wasm));
    assert!(owned_cases(&jvm) > 0);
}

#[test]
fn upstream_generator_parser_regressions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let result = Command::new("python3")
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "scripts",
            "-p",
            "test_box_selection.py",
        ])
        .current_dir(root)
        .output()
        .expect("run generator parser regressions");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
