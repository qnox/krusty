//! One box inventory: upstream cases and repository-owned portable regressions.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn regressions() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/box_regressions")
}

pub fn owned(file: &Path) -> bool {
    file.starts_with(regressions())
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

pub fn discover(corpus: &Path) -> Result<Vec<PathBuf>, String> {
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
    let upstream = krusty::conformance::kotlin_files(corpus);
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
        discover(&missing),
        Err(format!("box corpus is unavailable: {}", missing.display()))
    );
}
