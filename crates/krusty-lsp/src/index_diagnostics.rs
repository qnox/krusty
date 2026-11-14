use krusty::diag::{DiagnosticKind, Severity, Span};
use krusty_lsp::uri::path_to_file_uri;
use krusty_lsp::Analysis;

use super::WorkerHost;

#[test]
fn index_workspace_files_returns_exact_diagnostics_and_clears_the_latch() {
    let directory =
        std::env::temp_dir().join(format!("krusty-index-diagnostics-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("Broken.kt");
    let source = "fun answer(): Int = 42\nfun broken(value: Missing) = value\n";
    std::fs::write(&path, source).unwrap();
    let uri = path_to_file_uri(&path).unwrap();

    let options = krusty_lsp::LspOptions::parse(std::iter::empty::<String>()).unwrap();
    let classpath = options.effective_classpath();
    // The unit-test executable is the Cargo harness, which rejects `--analysis-worker`. The
    // supervisor binary is the one `index_workspace_files` execs in production. `cargo test --bin`
    // does not uplift that binary; the coverage harness builds it and publishes the path.
    let executable = supervisor_executable();
    let worker = krusty_lsp::AnalysisWorker::spawn(executable.clone(), classpath)
        .unwrap_or_else(|error| panic!("analysis worker {}: {error}", executable.display()));
    let mut host = WorkerHost::new(worker, options);
    assert!(!host.index_diagnostics_only);

    let outcome = host.index_workspace_files(&[&uri]);
    assert!(outcome.conclusive);
    assert!(
        !host.index_diagnostics_only,
        "a background chunk must clear the diagnostics-only latch before it returns"
    );
    assert_eq!(outcome.files.len(), 1);
    assert_eq!(outcome.files[0].uri, uri);
    let diagnostic = &outcome.files[0].diagnostics[0];
    assert_eq!(outcome.files[0].diagnostics.len(), 1);
    assert_eq!(diagnostic.file, 0);
    assert_eq!(diagnostic.span, Span::new(41, 48));
    assert_eq!(diagnostic.severity, Severity::Error);
    assert_eq!(diagnostic.kind, DiagnosticKind::Compiler);
    assert_eq!(diagnostic.msg, "unresolved reference 'Missing'.");

    let interactive_source = "fun answer(): Int = 42\n";
    let interactive_documents = [(uri.as_str(), interactive_source)];
    let interactive_uris = [uri.as_str()];
    let (interactive, _support) =
        host.analyze_open_documents(&interactive_documents, &interactive_uris);
    assert!(!host.index_diagnostics_only);
    assert_eq!(interactive.len(), 1);
    assert!(
        interactive[0].hover.entry_count() > 0
            && interactive[0].document_symbols.entry_count() > 0
            && interactive[0].definitions.entry_count() > 0,
        "interactive analysis after indexing must keep navigation indexes"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

/// The support probe returns before the chunk is analyzed. An empty module support set does not
/// call the worker; a support file on disk does, and a clean support compile is not a crash.
#[test]
fn support_probe_compiles_disk_support_and_skips_the_chunk() {
    let directory =
        std::env::temp_dir().join(format!("krusty-support-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let open_path = directory.join("Open.kt");
    std::fs::write(&open_path, "fun open(): Int = 1\n").unwrap();
    let open_uri = path_to_file_uri(&open_path).unwrap();

    let options = krusty_lsp::LspOptions::parse(std::iter::empty::<String>()).unwrap();
    let worker =
        krusty_lsp::AnalysisWorker::spawn(supervisor_executable(), options.effective_classpath())
            .expect("analysis worker");
    let mut host = WorkerHost::new(worker, options);
    host.set_workspace_root(Some(directory.clone()));

    let before = host.worker.completed_analysis_count();
    let empty = host.index_pass_kind(
        vec![(open_uri.clone(), "fun open(): Int = 1\n".to_string())],
        true,
    );
    assert!(!empty.crashed);
    assert!(empty.outcome.conclusive);
    assert!(empty.outcome.files.is_empty());
    assert_eq!(
        host.worker.completed_analysis_count(),
        before,
        "empty disk support must return before the worker runs"
    );

    let support_path = directory.join("Support.kt");
    std::fs::write(&support_path, "fun support(): Int = 1\n").unwrap();
    host.set_workspace_root(Some(directory.clone()));
    let before_support = host.worker.completed_analysis_count();
    let probed = host.index_pass_kind(
        vec![(open_uri.clone(), "fun open(): Int = 1\n".to_string())],
        true,
    );
    assert!(!probed.crashed);
    assert!(probed.outcome.conclusive);
    assert!(probed.outcome.files.is_empty());
    assert_eq!(
        host.worker.completed_analysis_count(),
        before_support + 1,
        "disk support must be compiled by the worker, not the empty short-circuit"
    );

    let indexed =
        host.index_pass_kind(vec![(open_uri, "fun open(): Int = 1\n".to_string())], false);
    assert!(!indexed.crashed);
    assert_eq!(indexed.outcome.files.len(), 1);
    assert!(indexed.outcome.files[0].diagnostics.is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

fn supervisor_executable() -> std::path::PathBuf {
    if let Some(published) = std::env::var_os("KRUSTY_LSP_BIN") {
        let published = std::path::PathBuf::from(published);
        assert!(
            published.is_file(),
            "KRUSTY_LSP_BIN does not name the supervisor: {}",
            published.display()
        );
        return published;
    }
    let current = std::env::current_exe().expect("current test executable");
    let mut dir = current.clone();
    for _ in 0..6 {
        if !dir.pop() {
            break;
        }
        let candidate = dir.join("krusty-lsp");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "krusty-lsp supervisor not found above {}; set KRUSTY_LSP_BIN to the built binary",
        current.display()
    );
}
