//! Crash isolation for one background index chunk.
//!
//! A chunk is analyzed as one source set. When the compiler panics, it panics on one construct in
//! one file, but the worker failure was charged to every file of the chunk: up to 31 files lost
//! their real diagnostics to "analysis worker failed". A crashed chunk is bisected instead, so only
//! the file that still crashes on its own keeps the failure diagnostic. Bisection runs only after a
//! crash and costs about two extra passes per halving.

/// One analysis pass over a chunk: the per-file results and whether the worker crashed (as
/// opposed to answering, or timing out, which bisection cannot help with).
pub(crate) struct ChunkPass {
    pub outcome: krusty_lsp::IndexOutcome,
    pub crashed: bool,
}

/// Index `readable` with `analyze`, bisecting any multi-file chunk the worker crashed on.
pub(crate) fn index_isolating_crashes(
    readable: Vec<(String, String)>,
    analyze: &mut impl FnMut(Vec<(String, String)>) -> ChunkPass,
) -> krusty_lsp::IndexOutcome {
    if readable.len() <= 1 {
        return analyze(readable).outcome;
    }
    let pass = analyze(readable.clone());
    if !pass.crashed {
        return pass.outcome;
    }
    let mut first = readable;
    let second = first.split_off(first.len() / 2);
    let mut outcome = index_isolating_crashes(first, analyze);
    let rest = index_isolating_crashes(second, analyze);
    outcome.files.extend(rest.files);
    outcome.conclusive &= rest.conclusive;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str) -> (String, String) {
        (format!("file:///w/{name}.kt"), format!("// {name}"))
    }

    /// A fake worker that crashes on any chunk containing `Bad`, and otherwise reports one
    /// diagnostic-free file per input, recording each pass it was asked to run.
    fn fake_worker(
        passes: &mut Vec<Vec<String>>,
    ) -> impl FnMut(Vec<(String, String)>) -> ChunkPass + '_ {
        move |chunk| {
            let uris = chunk.iter().map(|(uri, _)| uri.clone()).collect::<Vec<_>>();
            passes.push(uris.clone());
            let crashed = uris.iter().any(|uri| uri.ends_with("/Bad.kt"));
            let files = chunk
                .into_iter()
                .map(|(uri, text)| krusty_lsp::IndexedFile {
                    diagnostics: if crashed {
                        vec![krusty::diag::Diagnostic {
                            span: krusty::diag::Span::new(0, 0),
                            editor_span: None,
                            identity: None,
                            name: None,
                            severity: krusty::diag::Severity::Error,
                            kind: krusty::diag::DiagnosticKind::Compiler,
                            msg: "analysis worker failed: exited".to_string(),
                            file: 0,
                        }]
                    } else {
                        Vec::new()
                    },
                    text_hash: krusty_lsp::workspace_text_hash(&text),
                    uri,
                    text,
                })
                .collect();
            ChunkPass {
                outcome: krusty_lsp::IndexOutcome {
                    files,
                    conclusive: true,
                },
                crashed,
            }
        }
    }

    #[test]
    fn a_crash_is_charged_only_to_the_file_that_crashes_alone() {
        let chunk = ["A", "B", "Bad", "C"].map(source).to_vec();
        let mut passes = Vec::new();
        let outcome = index_isolating_crashes(chunk, &mut fake_worker(&mut passes));

        assert!(outcome.conclusive);
        let failures = outcome
            .files
            .iter()
            .map(|file| (file.uri.as_str(), file.diagnostics.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            failures,
            [
                ("file:///w/A.kt", 0),
                ("file:///w/B.kt", 0),
                ("file:///w/Bad.kt", 1),
                ("file:///w/C.kt", 0),
            ]
        );
        assert_eq!(
            passes,
            [
                vec![
                    "file:///w/A.kt",
                    "file:///w/B.kt",
                    "file:///w/Bad.kt",
                    "file:///w/C.kt"
                ],
                vec!["file:///w/A.kt", "file:///w/B.kt"],
                vec!["file:///w/Bad.kt", "file:///w/C.kt"],
                vec!["file:///w/Bad.kt"],
                vec!["file:///w/C.kt"],
            ]
        );
    }

    #[test]
    fn a_chunk_that_does_not_crash_runs_once() {
        let chunk = ["A", "B"].map(source).to_vec();
        let mut passes = Vec::new();
        let outcome = index_isolating_crashes(chunk, &mut fake_worker(&mut passes));

        assert_eq!(outcome.files.len(), 2);
        assert_eq!(passes.len(), 1);
    }
}
