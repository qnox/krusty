use std::io;

fn main() {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    let worker_mode = arguments
        .iter()
        .position(|argument| argument == "--analysis-worker")
        .map(|index| arguments.remove(index))
        .is_some();
    let options = krusty_lsp::LspOptions::parse(arguments.clone()).unwrap_or_else(|error| {
        eprintln!("krusty-lsp: {error}");
        std::process::exit(2);
    });
    if worker_mode {
        let stdin = io::stdin();
        let stdout = io::stdout();
        if let Err(error) = krusty_lsp::run_analysis_worker(
            &mut stdin.lock(),
            &mut stdout.lock(),
            options.effective_classpath(),
        ) {
            eprintln!("krusty-lsp worker: {error}");
            std::process::exit(1);
        }
        return;
    }

    let mut worker = krusty_lsp::AnalysisWorker::spawn(
        std::env::current_exe().expect("locate krusty-lsp executable"),
        arguments,
    )
    .unwrap_or_else(|error| {
        eprintln!("krusty-lsp: cannot start analysis worker: {error}");
        std::process::exit(1);
    });
    let analyze = move |sources: &[&str]| {
        worker.analyze(sources).unwrap_or_else(|error| {
            sources
                .iter()
                .map(|_| {
                    krusty_lsp::DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
                        span: krusty::diag::Span::new(0, 0),
                        severity: krusty::diag::Severity::Error,
                        msg: format!("analysis worker failed: {error}"),
                        file: 0,
                    }])
                })
                .collect()
        })
    };
    let result = krusty_lsp::run_stdio_connection_with(analyze);
    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("krusty-lsp: {error}");
            std::process::exit(1);
        }
    }
}
