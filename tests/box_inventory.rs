//! One box inventory: upstream cases and repository-owned portable regressions.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn regressions() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_regressions")
}

pub fn owned(file: &Path) -> bool {
    file.starts_with(regressions())
}

/// Locate the pinned Kotlin repository containing the canonical legacy box directory.
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

pub(super) fn generator_files(
    corpus: &Path,
    target: krusty::conformance::TestTarget,
) -> Result<Vec<PathBuf>, String> {
    use krusty::conformance::TestTarget;
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
            "upstream {target} selection failed: {}",
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
        .unwrap_or_else(|_| {
            file.strip_prefix(corpus)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| {
                    checkout(corpus)
                        .ok()
                        .and_then(|root| file.strip_prefix(root).ok())
                        .map(|relative| Path::new("upstream").join(relative))
                        .unwrap_or_else(|| file.to_path_buf())
                })
        });
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
        if file
            .strip_prefix(corpus)
            .is_ok_and(|path| path.starts_with("upstream"))
        {
            return Err(format!(
                "legacy box key uses reserved upstream namespace: {key}"
            ));
        }
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

pub fn discover(
    corpus: &Path,
    target: krusty::conformance::TestTarget,
) -> Result<Vec<PathBuf>, String> {
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
    let upstream = generator_files(corpus, target)?;
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

#[test]
fn discovery_retains_all_owned_cases_and_all_upstream_box_roots() {
    use krusty::conformance::TestTarget;
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned upstream corpus");
    let owned_count = krusty::conformance::kotlin_files(&regressions()).len();
    for target in [
        TestTarget::Jvm,
        TestTarget::Native,
        TestTarget::WasmJs,
        TestTarget::WasmWasi,
    ] {
        let selected = discover(&corpus, target).unwrap();
        assert_eq!(
            selected.iter().filter(|file| owned(file)).count(),
            owned_count
        );
        assert!(
            selected.iter().any(|file| key(&corpus, file)
                .starts_with("upstream/compiler/testData/codegen/boxInline/")),
            "{target:?}"
        );
        let jvm_only = selected
            .iter()
            .any(|file| key(&corpus, file).starts_with("compileKotlinAgainstKotlin/"));
        assert_eq!(
            jvm_only,
            matches!(target, TestTarget::Jvm | TestTarget::Native)
        );
    }
}

#[test]
fn extra_upstream_roots_have_stable_non_colliding_keys() {
    let corpus = Path::new("/checkout/compiler/testData/codegen/box");
    assert_eq!(key(corpus, &corpus.join("inline/a.kt")), "inline/a.kt");
    assert_eq!(
        key(
            corpus,
            Path::new("/checkout/compiler/testData/codegen/boxInline/inline/a.kt")
        ),
        "upstream/compiler/testData/codegen/boxInline/inline/a.kt"
    );
}

#[test]
fn wasm_outcome_lists_only_record_generator_selected_cases() {
    use super::box_ratchet::{self, Platform};
    for platform in [Platform::WasmJs, Platform::WasmWasi] {
        for version in krusty::kotlin_version::KotlinVersion::supported() {
            let baseline = box_ratchet::load(platform, version);
            let outside: Vec<_> = baseline
                .failures
                .union(&baseline.not_applicable)
                .filter(|key| key.starts_with("compileKotlinAgainstKotlin/"))
                .collect();
            assert_eq!(outside, Vec::<&String>::new(), "{platform:?} {version}");
        }
    }
}

#[test]
fn outcome_lists_only_record_files_in_the_complete_target_inventory() {
    use super::box_ratchet::{self, Platform};
    use krusty::conformance::TestTarget;
    let corpus = krusty::toolchain::box_corpus_dir().expect("provisioned Kotlin corpus");
    for (target, platform) in [
        (TestTarget::Jvm, Platform::Jvm),
        (TestTarget::Native, Platform::Native),
        (TestTarget::WasmJs, Platform::WasmJs),
        (TestTarget::WasmWasi, Platform::WasmWasi),
    ] {
        let selected: BTreeSet<_> = discover(&corpus, target)
            .unwrap()
            .iter()
            .map(|file| key(&corpus, file))
            .collect();
        let baseline = box_ratchet::load(platform, krusty::kotlin_version::target());
        let outside: Vec<_> = baseline
            .failures
            .union(&baseline.not_applicable)
            .filter(|key| !selected.contains(*key))
            .collect();
        assert_eq!(
            outside,
            Vec::<&String>::new(),
            "{platform:?}: outcome inventory includes an unselected upstream file"
        );
    }
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
        discover(&missing, krusty::conformance::TestTarget::Jvm),
        Err(format!("box corpus is unavailable: {}", missing.display()))
    );
}
