//! Compact semantic data retained by interactive language-server queries.

use std::collections::HashMap;

use krusty::diag::{Diagnostic, Span};
use krusty::types::Ty;
use krusty_analysis::{analyze_standalone_source_set, Analysis, FileAnalysis};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize, Serialize)]
struct HoverEntry {
    lo: u32,
    hi: u32,
    type_index: u32,
}

/// Compact semantic snapshot retained for hover queries after full compiler analysis is dropped.
#[derive(Default, Deserialize, Serialize)]
pub struct HoverIndex {
    entries: Vec<HoverEntry>,
    type_names: Vec<String>,
}

pub struct Hover<'a> {
    pub span: Span,
    pub type_name: &'a str,
}

impl HoverIndex {
    pub fn from_analysis(analysis: &Analysis) -> Self {
        Self::from_typed_expressions(analysis.typed_expressions(), analysis.file.expr_spans.len())
    }

    pub fn from_file_analysis(analysis: &FileAnalysis) -> Self {
        Self::from_typed_expressions(analysis.typed_expressions(), analysis.file.expr_spans.len())
    }

    fn from_typed_expressions(
        typed_expressions: impl Iterator<Item = (Span, Ty)>,
        capacity: usize,
    ) -> Self {
        let mut unique_types = Vec::new();
        let mut type_indices = HashMap::new();
        let mut entries = Vec::with_capacity(capacity);
        for (span, ty) in typed_expressions {
            if ty == Ty::Error {
                continue;
            }
            let type_index = match type_indices.get(&ty) {
                Some(&index) => index,
                None => {
                    let index = unique_types.len() as u32;
                    unique_types.push(ty);
                    type_indices.insert(ty, index);
                    index
                }
            };
            entries.push(HoverEntry {
                lo: span.lo,
                hi: span.hi,
                type_index,
            });
        }
        Self {
            entries,
            type_names: unique_types.into_iter().map(source_type_name).collect(),
        }
    }

    pub fn get(&self, byte_offset: u32) -> Option<Hover<'_>> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.lo <= byte_offset
                    && (byte_offset < entry.hi || (entry.lo == entry.hi && byte_offset == entry.lo))
            })
            .min_by_key(|entry| entry.hi.saturating_sub(entry.lo))
            .map(|entry| Hover {
                span: Span::new(entry.lo, entry.hi),
                type_name: &self.type_names[entry.type_index as usize],
            })
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn type_count(&self) -> usize {
        self.type_names.len()
    }
}

pub struct DocumentAnalysis {
    pub diagnostics: Vec<Diagnostic>,
    pub hover: HoverIndex,
}

impl DocumentAnalysis {
    pub fn from_analysis(analysis: Analysis) -> Self {
        let hover = HoverIndex::from_analysis(&analysis);
        Self {
            diagnostics: analysis.diagnostics,
            hover,
        }
    }

    pub fn from_file_analysis(analysis: FileAnalysis) -> Self {
        let hover = HoverIndex::from_file_analysis(&analysis);
        Self {
            diagnostics: analysis.diagnostics,
            hover,
        }
    }

    pub fn with_diagnostics(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            diagnostics,
            hover: HoverIndex::default(),
        }
    }

    pub fn empty() -> Self {
        Self::with_diagnostics(Vec::new())
    }
}

/// Analyze one source in an open source set and retain only data needed by editor queries.
pub fn analyze_for_lsp(sources: &[&str]) -> Vec<DocumentAnalysis> {
    analyze_standalone_source_set(sources)
        .files
        .into_iter()
        .map(DocumentAnalysis::from_file_analysis)
        .collect()
}

fn source_type_name(ty: Ty) -> String {
    match ty {
        Ty::Int => "Int".to_string(),
        Ty::Byte => "Byte".to_string(),
        Ty::Short => "Short".to_string(),
        Ty::Long => "Long".to_string(),
        Ty::Float => "Float".to_string(),
        Ty::Double => "Double".to_string(),
        Ty::Boolean => "Boolean".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::UInt => "UInt".to_string(),
        Ty::ULong => "ULong".to_string(),
        Ty::String => "String".to_string(),
        Ty::Unit => "Unit".to_string(),
        Ty::Obj(name, args) => {
            let mut rendered = name.render().replace('/', ".");
            if !args.is_empty() {
                rendered.push('<');
                for (index, arg) in args.iter().enumerate() {
                    if index != 0 {
                        rendered.push_str(", ");
                    }
                    rendered.push_str(&source_type_name(*arg));
                }
                rendered.push('>');
            }
            rendered
        }
        Ty::Null => "Nothing?".to_string(),
        Ty::Nothing => "Nothing".to_string(),
        Ty::Error => "<error>".to_string(),
        Ty::Fun(signature) => {
            let mut rendered = if signature.suspend {
                "suspend (".to_string()
            } else {
                "(".to_string()
            };
            for (index, parameter) in signature.params.iter().enumerate() {
                if index != 0 {
                    rendered.push_str(", ");
                }
                rendered.push_str(&source_type_name(*parameter));
            }
            rendered.push_str(") -> ");
            rendered.push_str(&source_type_name(signature.ret));
            rendered
        }
        Ty::Nullable(inner) => format!("{}?", source_type_name(*inner)),
        Ty::TyParam(name, _) => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_index_returns_smallest_typed_expression() {
        let source = "fun box(): Int { val answer = 40 + 2; return answer }";
        let mut analysis = analyze_standalone_source_set(&[source]).files;
        let analysis = analysis.remove(0);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );

        let index = HoverIndex::from_file_analysis(&analysis);
        let offset = source.rfind("answer").unwrap() as u32 + 1;
        let hover = index.get(offset).expect("hover over local read");
        assert_eq!(hover.type_name, "Int");
        assert_eq!(
            &source[hover.span.lo as usize..hover.span.hi as usize],
            "answer"
        );
    }

    #[test]
    fn hover_index_deduplicates_type_names_into_twelve_byte_entries() {
        let analysis = analyze_standalone_source_set(&["fun box(): Int = (40 + 2) * 1"])
            .files
            .remove(0);
        let index = HoverIndex::from_file_analysis(&analysis);
        assert!(index.entry_count() >= 5);
        assert_eq!(index.type_count(), 1);
        assert_eq!(std::mem::size_of::<HoverEntry>(), 12);
    }

    #[test]
    fn hover_formats_null_as_nullable_nothing() {
        let source = "fun box(): String? = null";
        let analysis = analyze_standalone_source_set(&[source]).files.remove(0);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        let index = HoverIndex::from_file_analysis(&analysis);
        let hover = index
            .get(source.rfind("null").unwrap() as u32 + 1)
            .expect("hover over null expression");
        assert_eq!(hover.type_name, "Nothing?");
    }
}
