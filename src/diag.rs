//! Diagnostics: spans plus messages, with line/column rendering.

use crate::types::TypeName;

/// A byte range into the source file. `u32` offsets keep this 8 bytes (data-oriented).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum DiagnosticKind {
    #[default]
    Compiler,
    IncompatibleEquality,
    Inspection,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DiagnosticIdentity {
    ClassifierAccess {
        reference: Span,
        classifier: TypeName,
    },
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Compiler-facing source range.
    pub span: Span,
    /// Optional editor-facing source range.
    pub editor_span: Option<Span>,
    pub severity: Severity,
    pub kind: DiagnosticKind,
    pub msg: String,
    pub identity: Option<DiagnosticIdentity>,
    /// Index of the source file this diagnostic belongs to (into the driver's `files`/`sources`
    /// lists). Diagnostics are produced one file at a time, so the sink stamps each with the file
    /// currently being processed — without it, a multi-file compile renders every error against the
    /// wrong file's source (bogus line numbers, foreign types). Single-file callers leave it 0.
    pub file: u32,
}

#[derive(Default)]
pub struct DiagSink {
    pub diags: Vec<Diagnostic>,
    /// The file index stamped onto subsequent diagnostics. The driver/front-end sets this before
    /// processing each file (see `set_file`); it stays 0 for the single-file box/test harness.
    current_file: u32,
}

impl DiagSink {
    pub fn new() -> DiagSink {
        DiagSink {
            diags: Vec::new(),
            current_file: 0,
        }
    }

    /// Stamp subsequent diagnostics as belonging to file `index` (the front-end calls this at the
    /// start of each per-file pass so errors carry their true origin in a multi-file compile).
    pub fn set_file(&mut self, index: u32) {
        self.current_file = index;
    }

    pub fn current_file(&self) -> u32 {
        self.current_file
    }

    pub fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.error_kind(span, DiagnosticKind::Compiler, msg);
    }

    pub fn error_kind(&mut self, span: Span, kind: DiagnosticKind, msg: impl Into<String>) {
        let msg = msg.into();
        crate::trace_compiler!(
            "diagnostic",
            "file={} span={span:?} kind={kind:?} message={msg}",
            self.current_file,
        );
        self.diags.push(Diagnostic {
            span,
            editor_span: None,
            severity: Severity::Error,
            kind,
            msg,
            identity: None,
            file: self.current_file,
        });
    }

    pub fn error_with_identity(
        &mut self,
        span: Span,
        identity: DiagnosticIdentity,
        msg: impl Into<String>,
    ) {
        self.diags.push(Diagnostic {
            span,
            editor_span: None,
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: msg.into(),
            identity: Some(identity),
            file: self.current_file,
        });
    }

    pub fn error_with_editor_span(
        &mut self,
        span: Span,
        editor_span: Span,
        msg: impl Into<String>,
    ) {
        self.diags.push(Diagnostic {
            span,
            editor_span: Some(editor_span),
            severity: Severity::Error,
            kind: DiagnosticKind::Compiler,
            msg: msg.into(),
            identity: None,
            file: self.current_file,
        });
    }

    pub fn collapse_duplicates(&mut self) {
        self.collapse_duplicates_from(0);
    }

    pub(crate) fn collapse_duplicates_from(&mut self, start: usize) {
        let start = start.min(self.diags.len());
        let mut tail = self.diags.split_off(start);
        let mut seen = std::collections::HashSet::with_capacity(tail.len());
        let mut seen_identities = std::collections::HashSet::new();
        tail.retain(|diagnostic| {
            if let Some(identity) = diagnostic.identity {
                seen_identities.insert((
                    diagnostic.file,
                    diagnostic.severity,
                    diagnostic.kind,
                    identity,
                ))
            } else {
                seen.insert((
                    diagnostic.file,
                    diagnostic.span,
                    diagnostic.editor_span,
                    diagnostic.severity,
                    diagnostic.kind,
                    diagnostic.msg.clone(),
                ))
            }
        });
        self.diags.extend(tail);
    }

    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| d.severity == Severity::Error)
    }

    /// Render `path:line:col: severity: msg` lines against the original source. Single-file callers:
    /// every diagnostic is rendered against `src` (its file index is assumed 0).
    pub fn render(&self, path: &str, src: &str) -> String {
        let mut out = String::new();
        for d in &self.diags {
            out.push_str(&self.render_one(d, path, src));
        }
        out
    }

    /// Render every diagnostic against ITS OWN source file (by `Diagnostic::file`), once. `files` is
    /// the driver's parallel `(path, source)` list. A diagnostic whose file index is out of range
    /// (defensive) falls back to the first file. This is the multi-file-correct renderer.
    pub fn render_all(&self, files: &[(&str, &str)]) -> String {
        let mut out = String::new();
        for d in &self.diags {
            let (path, src) = files
                .get(d.file as usize)
                .copied()
                .or_else(|| files.first().copied())
                .unwrap_or(("<unknown>", ""));
            out.push_str(&self.render_one(d, path, src));
        }
        out
    }

    fn render_one(&self, d: &Diagnostic, path: &str, src: &str) -> String {
        let (line, col) = line_col(src, d.span.lo);
        let sev = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        format!("{path}:{line}:{col}: {sev}: {}\n", d.msg)
    }
}

/// 1-based line and column for a byte offset.
pub fn line_col(src: &str, offset: u32) -> (usize, usize) {
    let off = (offset as usize).min(src.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, b) in src.bytes().enumerate() {
        if i >= off {
            break;
        }
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_basic() {
        let src = "ab\ncde\nf";
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 1), (1, 2));
        assert_eq!(line_col(src, 3), (2, 1)); // 'c'
        assert_eq!(line_col(src, 7), (3, 1)); // 'f'
    }

    #[test]
    fn render_includes_location() {
        let mut s = DiagSink::new();
        s.error(Span::new(3, 4), "boom");
        let r = s.render("X.kt", "ab\ncde");
        assert!(r.contains("X.kt:2:1: error: boom"), "got: {r}");
        assert!(s.has_errors());
    }

    #[test]
    fn identical_diagnostics_collapse() {
        let mut s = DiagSink::new();
        s.error(Span::new(3, 4), "boom");
        s.error(Span::new(3, 4), "boom");
        s.error(Span::new(5, 6), "boom");
        s.error(Span::new(3, 4), "other");
        s.set_file(1);
        s.error(Span::new(3, 4), "boom");

        s.collapse_duplicates();

        assert_eq!(s.diags.len(), 4, "{:?}", s.diags);
    }

    #[test]
    fn semantic_identity_collapse_preserves_first_spelling() {
        let mut s = DiagSink::new();
        let identity = DiagnosticIdentity::ClassifierAccess {
            reference: Span::new(3, 4),
            classifier: crate::types::type_name("scope/Classifier"),
        };
        s.error_with_identity(Span::new(3, 4), identity, "cannot access 'Alias'");
        s.error_with_identity(
            Span::new(3, 4),
            identity,
            "cannot access 'scope.Classifier'",
        );
        s.error_with_identity(
            Span::new(5, 6),
            DiagnosticIdentity::ClassifierAccess {
                reference: Span::new(5, 6),
                classifier: crate::types::type_name("scope/Classifier"),
            },
            "cannot access 'SecondAlias'",
        );

        s.collapse_duplicates();

        assert_eq!(s.diags.len(), 2);
        assert_eq!(s.diags[0].msg, "cannot access 'Alias'");
        assert_eq!(s.diags[1].msg, "cannot access 'SecondAlias'");
    }

    #[test]
    fn speculative_probes_still_see_every_emission() {
        let mut s = DiagSink::new();
        s.error(Span::new(3, 4), "boom");
        let checkpoint = s.diags.len();
        s.error(Span::new(3, 4), "boom");
        assert!(
            s.diags.len() > checkpoint,
            "the probe must observe its own error"
        );
        s.diags.truncate(checkpoint);
        assert_eq!(s.diags.len(), 1);
    }

    #[test]
    fn ranged_collapse_preserves_existing_diagnostics() {
        let mut s = DiagSink::new();
        s.error(Span::new(3, 4), "boom");
        s.error(Span::new(3, 4), "boom");
        let start = s.diags.len();
        s.error(Span::new(5, 6), "later");
        s.error(Span::new(5, 6), "later");

        s.collapse_duplicates_from(start);

        assert_eq!(s.diags.len(), 3);
    }

    #[test]
    fn render_all_attributes_each_diag_to_its_own_file() {
        // Two files; an error in each. `render_all` must render each against ITS OWN source — not the
        // whole list against every file (the multi-file mis-attribution bug).
        let mut s = DiagSink::new();
        s.set_file(0);
        s.error(Span::new(0, 1), "in A"); // offset 0 → A.kt:1:1
        s.set_file(1);
        s.error(Span::new(4, 5), "in B"); // offset 4 → line 2 of B's source
        let files = [("A.kt", "xyz"), ("B.kt", "ab\ncde")];
        let r = s.render_all(&files);
        assert!(r.contains("A.kt:1:1: error: in A"), "got: {r}");
        assert!(r.contains("B.kt:2:2: error: in B"), "got: {r}");
        // Exactly two lines — no duplication across files.
        assert_eq!(r.lines().count(), 2, "got: {r}");
    }
}
