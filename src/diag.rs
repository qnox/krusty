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
}

#[derive(Default)]
pub struct DiagSink {
    pub diags: Vec<Diagnostic>,
}

impl DiagSink {
    pub fn new() -> DiagSink {
        DiagSink { diags: Vec::new() }
    }

    pub fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic {
            span,
            severity: Severity::Error,
            msg: msg.into(),
        });
    }

    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| d.severity == Severity::Error)
    }

    /// Render `path:line:col: severity: msg` lines against the original source.
    pub fn render(&self, path: &str, src: &str) -> String {
        let mut out = String::new();
        for d in &self.diags {
            let (line, col) = line_col(src, d.span.lo);
            let sev = match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            out.push_str(&format!("{path}:{line}:{col}: {sev}: {}\n", d.msg));
        }
        out
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
}
