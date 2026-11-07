//! Module-qualified KLIB entry scoring in the runnable platform/version box matrix.
mod emission;
mod emission_capture;
pub(super) use emission::backend as metadata_backend;
pub(super) use emission_capture::{
    active as capturing, record as capture_artifacts, MetadataObserver,
};
mod reference;
mod reference_cache;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use krusty::conformance::{ModuleUnit, TestTarget};

use super::box_byte_score::{self, ByteScore, CaseScore, QualifiedArtifacts};
use super::box_lane::{self, Lane, Outcome};

pub(super) struct ByteRun {
    report: Option<PathBuf>,
    results: Mutex<Vec<(PathBuf, CaseScore)>>,
}

impl ByteRun {
    pub fn new(lane: &dyn Lane) -> Self {
        Self {
            report: std::env::var_os(format!("KRUSTY_{}_BYTE_REPORT", lane.env_infix()))
                .map(PathBuf::from),
            results: Mutex::new(Vec::new()),
        }
    }

    pub fn capture(&self) -> emission_capture::Capture {
        emission_capture::Capture::new(self.report.is_some())
    }

    pub fn record(
        &self,
        lane: &dyn Lane,
        file: &Path,
        source: Result<&str, &std::io::Error>,
        outcome: &Outcome,
        capture: emission_capture::Capture,
    ) {
        if self.report.is_none() || matches!(outcome, Outcome::NotApplicable(_)) {
            return;
        }
        let actual = capture.finish();
        let result = match source {
            Ok(source) => score(lane.test_target(), file, source, outcome, actual),
            Err(error) => CaseScore::RefFail(format!("read source: {error}")),
        };
        self.results.lock().unwrap().push((file.to_owned(), result));
    }

    pub fn finish(self) {
        let Some(path) = self.report else { return };
        let mut total = ByteScore::default();
        let mut errors = Vec::new();
        let results = self.results.into_inner().unwrap();
        for (file, result) in &results {
            match result {
                CaseScore::Scored(score) => total.add(*score),
                CaseScore::RefFail(error) => errors.push(format!("{}: {error}", file.display())),
                CaseScore::NotScored => unreachable!("only applicable scored cases are recorded"),
            }
        }
        super::common::byte_dump::flush_recorded_class_dumps();
        let (hits, misses) = reference::cache_stats();
        eprintln!("KLIB references: cache hits {hits}, misses {misses}");
        assert!(
            errors.is_empty(),
            "KLIB byte reference failures:\n{}",
            errors.join("\n")
        );
        let report = super::common::conformance_report::count_report(total.matched, total.total);
        std::fs::write(&path, &report).expect("write KLIB byte report");
        eprintln!(
            "KLIB byte equality: {report} ({} applicable cases)",
            results.len()
        );
    }
}

fn score(
    target: TestTarget,
    file: &Path,
    source: &str,
    outcome: &Outcome,
    actual: Result<QualifiedArtifacts, String>,
) -> CaseScore {
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("Case");
    let units = match units(source, stem, target) {
        Ok(units) => units,
        Err(error) => return CaseScore::RefFail(error),
    };
    let reference = reference::compile(target, source, &units).map(|(entries, _)| entries);
    let actual = if matches!(outcome, Outcome::Pass) {
        // A KLIB emission failure earns no byte credit; it cannot change the execution verdict.
        actual.unwrap_or_else(|error| {
            eprintln!("KLIB emission {}: {error}", file.display());
            QualifiedArtifacts::new()
        })
    } else {
        QualifiedArtifacts::new()
    };
    box_byte_score::score_case(
        reference.as_ref().map_err(String::as_str),
        matches!(outcome, Outcome::Pass),
        &actual,
    )
}

fn units(source: &str, stem: &str, target: TestTarget) -> Result<Vec<ModuleUnit>, String> {
    if source.contains("// MODULE:") {
        let mut modules = reference::module_blocks(source, target)?;
        if krusty::conformance::directive(source, "WITH_COROUTINES") {
            krusty::conformance::inject_support_module(
                &mut modules,
                krusty::conformance::COROUTINE_HELPERS,
            );
        }
        Ok(krusty::conformance::module_units(&modules))
    } else {
        let files = box_lane::prepare_sources(source, stem, target)
            .map_err(|error| format!("{error:?}"))?;
        Ok(vec![ModuleUnit {
            name: "box".into(),
            deps: vec![],
            friends: vec![],
            files,
            common_file_count: 0,
            java_files: vec![],
            fragments: vec![],
        }])
    }
}

/// Resolve the declared dependency closure without flattening independently built modules.
fn dependencies(
    unit: &ModuleUnit,
    built: &BTreeMap<String, BuiltModule>,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let mut libraries = Vec::new();
    let mut friends = Vec::new();
    for name in &unit.deps {
        let dependency = built
            .get(name)
            .ok_or_else(|| format!("module dependency {name} was not built"))?;
        for path in std::iter::once(&dependency.path).chain(&dependency.dependencies) {
            if !libraries.contains(path) {
                libraries.push(path.clone());
            }
        }
        if unit.friends.contains(name) {
            friends.push(dependency.path.clone());
        }
    }
    Ok((libraries, friends))
}

struct BuiltModule {
    path: PathBuf,
    dependencies: Vec<PathBuf>,
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, String> {
        super::common::scratch_dir()
            .map(Self)
            .ok_or_else(|| "cannot allocate KLIB scratch directory".into())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests;

/// The runnable Native lane targets Linux on the runner's architecture. Library comparison uses
/// that same explicit target even when its metadata-only smoke test runs on another host OS.
fn native_target() -> Result<krusty::native::NativeTarget, String> {
    Ok(krusty::native::NativeTarget::new(
        krusty::native::Arch::host().ok_or("unsupported Native architecture")?,
        krusty::native::Os::Linux,
    ))
}
