//! Crash isolation for one background index chunk.
//!
//! A chunk is analyzed as one source set. When the compiler panics on one construct in one of the
//! chunk's files, charging the failure to every file of the chunk loses the others' diagnostics,
//! so a crashed chunk is bisected and only a file that still crashes on its own keeps the failure.
//!
//! The worker analyzes more than the chunk's files: the rest of their module comes along as support
//! sources whichever half is analyzed. A crash in that shared part follows every half, so when both
//! halves of a split crash, the crash is not attributable to the chunk's files and bisection stops
//! there, charging the failure to each of them. Isolating one crashing file costs about two passes
//! per halving; a shared crash costs three passes in total.

/// One analysis pass over a chunk: the per-file results and whether the worker crashed (as
/// opposed to answering, or timing out, which bisection cannot help with).
pub(crate) struct ChunkPass {
    pub outcome: krusty_lsp::IndexOutcome,
    pub crashed: bool,
}

/// Index `readable` with `analyze`, isolating the files of a chunk the worker crashed on.
pub(crate) fn index_isolating_crashes(
    readable: Vec<(String, String)>,
    analyze: &mut impl FnMut(Vec<(String, String)>) -> ChunkPass,
) -> krusty_lsp::IndexOutcome {
    let pass = analyze(readable.clone());
    if !pass.crashed || readable.len() <= 1 {
        return pass.outcome;
    }
    isolate_crash(readable, analyze)
}

/// Bisect a chunk of several files whose pass crashed.
fn isolate_crash(
    mut readable: Vec<(String, String)>,
    analyze: &mut impl FnMut(Vec<(String, String)>) -> ChunkPass,
) -> krusty_lsp::IndexOutcome {
    let second = readable.split_off(readable.len() / 2);
    let halves = [readable, second].map(|half| {
        let pass = analyze(half.clone());
        (half, pass)
    });
    let shared_crash = halves.iter().all(|(_, pass)| pass.crashed);
    let mut outcome = krusty_lsp::IndexOutcome {
        files: Vec::new(),
        conclusive: true,
    };
    for (half, pass) in halves {
        let half = if pass.crashed && !shared_crash && half.len() > 1 {
            isolate_crash(half, analyze)
        } else {
            pass.outcome
        };
        outcome.files.extend(half.files);
        outcome.conclusive &= half.conclusive;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str) -> (String, String) {
        (format!("file:///w/{name}.kt"), format!("// {name}"))
    }

    /// A fake worker that crashes on any chunk containing `Bad`.
    fn fake_worker(
        passes: &mut Vec<Vec<String>>,
    ) -> impl FnMut(Vec<(String, String)>) -> ChunkPass + '_ {
        worker_crashing_when(passes, |uris| {
            uris.iter().any(|uri| uri.ends_with("/Bad.kt"))
        })
    }

    /// A fake worker that crashes on the chunks `crashes` selects, reporting the failure on each
    /// file of such a chunk and a diagnostic-free file otherwise, recording each pass it runs.
    fn worker_crashing_when<'a>(
        passes: &'a mut Vec<Vec<String>>,
        crashes: impl Fn(&[String]) -> bool + 'a,
    ) -> impl FnMut(Vec<(String, String)>) -> ChunkPass + 'a {
        move |chunk| {
            let uris = chunk.iter().map(|(uri, _)| uri.clone()).collect::<Vec<_>>();
            passes.push(uris.clone());
            let crashed = crashes(&uris);
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

    /// A crash in what every pass shares, such as the module's support sources, follows both halves
    /// of the first split; bisection stops there instead of analyzing every file alone.
    #[test]
    fn a_crash_every_pass_shares_stops_after_one_split() {
        let names = (0..32).map(|index| format!("F{index}")).collect::<Vec<_>>();
        let chunk = names.iter().map(|name| source(name)).collect::<Vec<_>>();
        let mut passes = Vec::new();
        let outcome =
            index_isolating_crashes(chunk, &mut worker_crashing_when(&mut passes, |_| true));

        assert_eq!(
            passes.iter().map(Vec::len).collect::<Vec<_>>(),
            [32, 16, 16]
        );
        assert_eq!(outcome.files.len(), 32);
        assert!(outcome.files.iter().all(|file| file.diagnostics.len() == 1));
    }
}
