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
    // supervisor binary is the one `index_workspace_files` execs in production.
    let mut executable = std::env::current_exe().unwrap();
    assert!(
        executable.pop() && executable.pop(),
        "test executable layout"
    );
    executable.push("krusty-lsp");
    let worker = krusty_lsp::AnalysisWorker::spawn(executable, classpath).expect("analysis worker");
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

    let interactive = host.analyze(&["fun answer(): Int = 42\n"]);
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
