//! Crash isolation for one background index chunk.
//!
//! A chunk is analyzed as one source set. When the compiler panics on one construct in one of the
//! chunk's files, charging the failure to every file of the chunk loses the others' diagnostics,
//! so a crashed chunk is bisected and only a file that still crashes on its own keeps the failure.
//!
//! The worker also analyzes the rest of the chunk's module as support sources, and those accompany
//! every half. A crash in that support follows every subset, so splitting the chunk cannot recover
//! a file's diagnostics. Both halves crashing is not that evidence: each half may contain a file
//! that crashes on its own. The support sources are compiled once without the chunk's files, and
//! bisection stops only when that pass crashes. Otherwise each crashing half is isolated on its
//! own. One crashing file costs about two passes per halving; a support crash costs the original
//! pass plus the support pass.

/// One analysis pass over a chunk: the per-file results and whether the worker crashed (as
/// opposed to answering, or timing out, which bisection cannot help with).
pub(crate) struct ChunkPass {
    pub outcome: krusty_lsp::IndexOutcome,
    pub crashed: bool,
}

/// What one isolation pass analyzes.
pub(crate) enum IsolationRequest {
    /// This subset of the chunk. The worker still attaches the module's support sources.
    Files(Vec<(String, String)>),
    /// The module support that accompanies this chunk, without the chunk's own files.
    Support(Vec<(String, String)>),
}

/// Index `readable` with `analyze`, isolating the files of a chunk the worker crashed on.
pub(crate) fn index_isolating_crashes(
    readable: Vec<(String, String)>,
    analyze: &mut impl FnMut(IsolationRequest) -> ChunkPass,
) -> krusty_lsp::IndexOutcome {
    let pass = analyze(IsolationRequest::Files(readable.clone()));
    if !pass.crashed || readable.len() <= 1 {
        return pass.outcome;
    }
    // Support that crashes on its own crashes every subset. File diagnostics cannot be recovered.
    if analyze(IsolationRequest::Support(readable.clone())).crashed {
        return pass.outcome;
    }
    bisect(readable, analyze)
}

/// Bisect a chunk of several files whose crash is not in the shared support sources.
fn bisect(
    mut readable: Vec<(String, String)>,
    analyze: &mut impl FnMut(IsolationRequest) -> ChunkPass,
) -> krusty_lsp::IndexOutcome {
    let second = readable.split_off(readable.len() / 2);
    let mut outcome = isolate_half(readable, analyze);
    let rest = isolate_half(second, analyze);
    outcome.files.extend(rest.files);
    outcome.conclusive &= rest.conclusive;
    outcome
}

/// Analyze one half, and split it again when the crash is still inside it.
fn isolate_half(
    readable: Vec<(String, String)>,
    analyze: &mut impl FnMut(IsolationRequest) -> ChunkPass,
) -> krusty_lsp::IndexOutcome {
    let pass = analyze(IsolationRequest::Files(readable.clone()));
    if !pass.crashed || readable.len() <= 1 {
        return pass.outcome;
    }
    bisect(readable, analyze)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str) -> (String, String) {
        (format!("file:///w/{name}.kt"), format!("// {name}"))
    }

    /// A fake worker that crashes on any chunk containing `Bad`, and whose support sources do not.
    fn fake_worker(
        passes: &mut Vec<Vec<String>>,
    ) -> impl FnMut(IsolationRequest) -> ChunkPass + '_ {
        worker_crashing_when(
            passes,
            |uris| uris.iter().any(|uri| uri.ends_with("/Bad.kt")),
            false,
        )
    }

    /// The pass list records a support-only confirmation as this single entry.
    fn support_pass() -> Vec<String> {
        vec!["<support>".to_string()]
    }

    /// A fake worker that crashes on the file sets `crashes` selects, and on the support-only
    /// pass when `support_crashes` is set. A crashing file set reports the failure on each of its
    /// files; any other set is diagnostic-free. Each pass is recorded.
    fn worker_crashing_when<'a>(
        passes: &'a mut Vec<Vec<String>>,
        crashes: impl Fn(&[String]) -> bool + 'a,
        support_crashes: bool,
    ) -> impl FnMut(IsolationRequest) -> ChunkPass + 'a {
        move |request| {
            let (chunk, crashed) = match request {
                IsolationRequest::Support(_) => {
                    passes.push(support_pass());
                    (Vec::new(), support_crashes)
                }
                IsolationRequest::Files(chunk) => {
                    let uris = chunk.iter().map(|(uri, _)| uri.clone()).collect::<Vec<_>>();
                    let crashed = crashes(&uris);
                    passes.push(uris);
                    (chunk, crashed)
                }
            };
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
                vec!["<support>"],
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

    /// Support that crashes on its own is the shared failure. Halves are not analyzed: every subset
    /// would crash the same way, and the chunk's files keep the failure from the first pass.
    #[test]
    fn a_crash_in_shared_support_stops_without_bisecting() {
        let names = (0..32).map(|index| format!("F{index}")).collect::<Vec<_>>();
        let chunk = names.iter().map(|name| source(name)).collect::<Vec<_>>();
        let mut passes = Vec::new();
        let outcome = index_isolating_crashes(
            chunk,
            &mut worker_crashing_when(&mut passes, |_| true, true),
        );

        assert_eq!(
            passes.iter().map(Vec::len).collect::<Vec<_>>(),
            [32, support_pass().len()]
        );
        assert_eq!(
            passes.last().map(Vec::as_slice),
            Some(support_pass().as_slice())
        );
        assert_eq!(outcome.files.len(), 32);
        assert!(outcome.files.iter().all(|file| file.diagnostics.len() == 1));
    }

    /// Two files that each crash, placed in opposite halves, are not one shared crash. The healthy
    /// files keep the diagnostics their own passes produced.
    #[test]
    fn independent_crashes_in_opposite_halves_are_isolated() {
        let chunk = ["Bad1", "Healthy1", "Bad2", "Healthy2"]
            .map(source)
            .to_vec();
        let mut passes = Vec::new();
        let outcome = index_isolating_crashes(
            chunk,
            &mut worker_crashing_when(
                &mut passes,
                |uris| {
                    uris.iter()
                        .any(|uri| uri.ends_with("/Bad1.kt") || uri.ends_with("/Bad2.kt"))
                },
                false,
            ),
        );

        assert!(outcome.conclusive);
        let failures = outcome
            .files
            .iter()
            .map(|file| (file.uri.as_str(), file.diagnostics.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            failures,
            [
                ("file:///w/Bad1.kt", 1),
                ("file:///w/Healthy1.kt", 0),
                ("file:///w/Bad2.kt", 1),
                ("file:///w/Healthy2.kt", 0),
            ]
        );
        assert_eq!(
            passes,
            [
                vec![
                    "file:///w/Bad1.kt",
                    "file:///w/Healthy1.kt",
                    "file:///w/Bad2.kt",
                    "file:///w/Healthy2.kt",
                ],
                vec!["<support>"],
                vec!["file:///w/Bad1.kt", "file:///w/Healthy1.kt"],
                vec!["file:///w/Bad1.kt"],
                vec!["file:///w/Healthy1.kt"],
                vec!["file:///w/Bad2.kt", "file:///w/Healthy2.kt"],
                vec!["file:///w/Bad2.kt"],
                vec!["file:///w/Healthy2.kt"],
            ]
        );
    }
}
