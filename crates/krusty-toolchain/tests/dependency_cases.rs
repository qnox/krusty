//! `show dependencies` prints exactly what JetBrains' `kotlin show dependencies --all-modules
//! --include-tests` printed for each recorded project, resolving from the same local Maven
//! repository and nothing else. The cases are in `tests/recorded/dependencies`, with the artifacts
//! every case resolves (the Kotlin standard library and the test framework) in its `repository`;
//! re-record them with `scripts/kotlin-toolchain/record_projects.py --dependencies`.

mod support;

use std::path::{Path, PathBuf};

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::maven::{Metadata, Store};
use krusty_toolchain::{configuration, model, resolution, show};

use support::{materialize, reported, Case, REPOSITORY_SECTION};

/// The case's local Maven repository, beside its project as the recorder makes it: the shared
/// `tests/recorded/dependencies/repository` with the case's `m2/<path>` sections added.
fn local_repository(case: &Case, beside: &Path) -> PathBuf {
    let local = beside.join("m2");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded/dependencies/repository"),
        &local,
    );
    for (file, text) in &case.files {
        if let Some(file) = file.strip_prefix(REPOSITORY_SECTION) {
            let path = local.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }
    local
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn every_recorded_dependency_case_is_resolved_as_the_toolchain_resolves_it() {
    let cases = support::cases("dependencies", &["expected", "dependencies"]);
    assert_eq!(cases.len(), 6, "the recorded cases are missing");
    let mut failures = Vec::new();
    for case in &cases {
        let (_temp, root) = materialize("dependencies", case);
        let beside = root.parent().unwrap();
        let local = local_repository(case, beside);
        let mut diagnostics = Diagnostics::default();
        let model = model::read(model::Start::Discover(&root), &mut diagnostics)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name))
            .unwrap_or_else(|| panic!("{}: the project was not read", case.name));
        let configured = configuration::configure(&root, &model.modules, &mut diagnostics);
        let actual = reported(&root, &diagnostics);
        let expected = case.section("expected").cloned().unwrap_or_default();
        if actual != expected {
            failures.push(format!(
                "{}:\n  expected {expected:#?}\n  actual   {actual:#?}",
                case.name
            ));
            continue;
        }
        let declarations = resolution::read_declarations(&model.modules, &configured)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        // An empty cache: everything is read from the case's repository.
        let store = Store::with_local(&local, &beside.join("cache"));
        let metadata = Metadata::new(&store);
        let mut resolvers = resolution::Resolvers::new(&metadata);
        let mut printed = String::new();
        for (index, module) in model.modules.iter().enumerate() {
            let (mut graphs, resolver) = resolvers.resolve_module(&declarations, index, true);
            printed.push_str(&show::module_dependencies(
                &module.name,
                &mut graphs,
                resolver,
            ));
        }
        let mut lines: Vec<String> = printed
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if Some(&lines) != case.section("dependencies") {
            failures.push(format!(
                "{}: printed\n{}\nrecorded\n{}",
                case.name,
                lines.join("\n"),
                case.section("dependencies")
                    .cloned()
                    .unwrap_or_default()
                    .join("\n")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
