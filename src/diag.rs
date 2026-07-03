//! Diagnostics: spans plus messages, with line/column rendering.

/// A byte range into the source file. `u32` offsets keep this 8 bytes (data-oriented).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub span: Span,
    pub severity: Severity,
    pub msg: String,
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

    pub fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic {
            span,
            severity: Severity::Error,
            msg: msg.into(),
            file: self.current_file,
        });
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
    fn render_one_labels_warning_severity() {
        // `error(...)` only produces Error diagnostics, so build a Warning by hand to hit that arm.
        let s = DiagSink::new();
        let d = Diagnostic {
            span: Span::new(0, 1),
            severity: Severity::Warning,
            msg: "heads up".to_string(),
            file: 0,
        };
        assert_eq!(
            s.render_one(&d, "W.kt", "abc"),
            "W.kt:1:1: warning: heads up\n"
        );
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
