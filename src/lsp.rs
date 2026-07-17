//! LSP-facing analysis helpers.

use crate::ast::File;
use crate::diag::{DiagSink, Diagnostic};
use crate::frontend::{self, FrontendSymbols, FrontendTypeInfo};
use crate::libraries::SemanticPlatform;

#[derive(Clone, Debug)]
pub struct Document {
    pub uri: String,
    pub text: String,
}

impl Document {
    pub fn new(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            text: text.into(),
        }
    }
}

pub struct Analysis {
    pub uri: String,
    pub file: File,
    pub symbols: Option<FrontendSymbols>,
    pub types: Option<FrontendTypeInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == crate::diag::Severity::Error)
    }

    pub fn diagnostic_messages(&self) -> Vec<&str> {
        self.diagnostics.iter().map(|d| d.msg.as_str()).collect()
    }
}

/// Analyze one open document against a semantic library provider.
pub fn analyze_document(doc: &Document, platform: Box<dyn SemanticPlatform>) -> Analysis {
    let mut diags = DiagSink::new();
    let (file, symbols, types) = frontend::analyze_source(&doc.text, platform, &mut diags);
    Analysis {
        uri: doc.uri.clone(),
        file,
        symbols,
        types,
        diagnostics: diags.diags,
    }
}

/// Analyze one open document without external libraries.
pub fn analyze_standalone_document(doc: &Document) -> Analysis {
    let mut diags = DiagSink::new();
    let (file, symbols, types) = frontend::analyze_source_standalone(&doc.text, &mut diags);
    Analysis {
        uri: doc.uri.clone(),
        file,
        symbols,
        types,
        diagnostics: diags.diags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_analysis_accepts_memory_document() {
        let doc = Document::new("file:///main.kt", "fun box(): String = \"OK\"");
        let analysis = analyze_standalone_document(&doc);
        assert_eq!(analysis.uri, "file:///main.kt");
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        assert!(analysis.symbols.is_some());
        assert!(analysis.types.is_some());
    }

    #[test]
    fn lsp_analysis_reports_memory_document_diagnostics() {
        let doc = Document::new("file:///main.kt", "fun box(): Int = \"no\"");
        let analysis = analyze_standalone_document(&doc);
        assert!(analysis.has_errors());
        assert!(
            analysis
                .diagnostic_messages()
                .iter()
                .any(|msg| msg.contains("return type mismatch")),
            "{:?}",
            analysis.diagnostic_messages()
        );
    }
}
