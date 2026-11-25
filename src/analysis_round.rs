//! What an Analysis API host reads for one session: each source file's declarations as positions,
//! built from the parsed files' declaration outline. See `docs/PLUGIN_API.md`.
//!
//! Positions are the only fact the round carries so far. They are measured as kotlinc measures
//! them (UTF-16 units of the text kotlinc reads), so the host never converts an offset.

use krusty_plugin_api::analysis::{
    AnalysisDeclaration, AnalysisDeclarationKind, AnalysisFile, AnalysisRange, AnalysisSession,
};

use crate::diag::kotlinc_offsets::Utf16Offsets;
use crate::diag::Span;
use crate::frontend::{DeclarationOutline, OutlineKind, SourceSetAnalysis};

/// The session over `analysis`, whose files were parsed from `texts` and are at `paths`, one each
/// in the analysis's order.
pub fn analysis_round(
    analysis: &SourceSetAnalysis,
    paths: &[String],
    texts: &[&str],
) -> Result<AnalysisSession, String> {
    if paths.len() != analysis.files.len() || texts.len() != analysis.files.len() {
        return Err(format!(
            "The Analysis API needs the path and text of every source: {} paths and {} texts were \
             given for {} sources",
            paths.len(),
            texts.len(),
            analysis.files.len()
        ));
    }
    let files = analysis
        .files
        .iter()
        .zip(paths)
        .zip(texts)
        .map(|((file, path), text)| {
            let offsets = Utf16Offsets::new(text);
            let range = |span: Span| AnalysisRange {
                start: offsets.unit(span.lo),
                end: offsets.unit(span.hi),
            };
            let declarations = DeclarationOutline::build(file)
                .nodes()
                .iter()
                .map(|node| AnalysisDeclaration {
                    kind: kind(node.kind),
                    name: node.name.to_string(),
                    name_range: range(node.name_span),
                    range: range(node.range),
                    parent: node.parent,
                })
                .collect();
            AnalysisFile {
                path: path.clone(),
                declarations,
            }
        })
        .collect();
    Ok(AnalysisSession { files })
}

fn kind(kind: OutlineKind) -> AnalysisDeclarationKind {
    match kind {
        OutlineKind::Class => AnalysisDeclarationKind::Class,
        OutlineKind::Interface => AnalysisDeclarationKind::Interface,
        OutlineKind::Enum => AnalysisDeclarationKind::Enum,
        OutlineKind::Annotation => AnalysisDeclarationKind::Annotation,
        OutlineKind::Object => AnalysisDeclarationKind::Object,
        OutlineKind::EnumEntry => AnalysisDeclarationKind::EnumEntry,
        OutlineKind::Function => AnalysisDeclarationKind::Function,
        OutlineKind::Property => AnalysisDeclarationKind::Property,
        OutlineKind::TypeAlias => AnalysisDeclarationKind::TypeAlias,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::features::LangFeatures;
    use crate::frontend::{analyze_source_set_prefix_with_features, PlatformProvider, SourceInput};
    use crate::libraries::EmptySymbolSource;

    fn session(sources: &[(&str, &str)]) -> AnalysisSession {
        let inputs: Vec<_> = sources
            .iter()
            .map(|(_, text)| SourceInput::kotlin(text))
            .collect();
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            0,
            inputs.len(),
            PlatformProvider::jvm(Box::new(EmptySymbolSource)),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        let paths: Vec<_> = sources.iter().map(|(path, _)| path.to_string()).collect();
        let texts: Vec<_> = sources.iter().map(|(_, text)| *text).collect();
        analysis_round(&analysis, &paths, &texts).unwrap()
    }

    /// The text kotlinc reads at `range`, in UTF-16 units.
    fn slice(text: &str, range: AnalysisRange) -> String {
        let units: Vec<u16> = crate::diag::kotlinc_offsets::kotlinc_text(text)
            .encode_utf16()
            .collect();
        String::from_utf16(&units[range.start as usize..range.end as usize]).unwrap()
    }

    #[test]
    fn every_name_range_selects_the_declared_name_whatever_the_line_breaks_and_characters_before() {
        for separator in ["\n", "\r\n", "\r"] {
            let text = format!(
                "package p{separator}{separator}val first = \"é\u{1F600}\"{separator}class Box {{{separator}    fun \
                 öpen() = 1{separator}}}{separator}val s = \"\u{1F600}\"{separator}val top = 2{separator}"
            );
            let session = session(&[("a.kt", &text)]);
            let file = &session.files[0];
            let names: Vec<_> = file
                .declarations
                .iter()
                .map(|declaration| slice(&text, declaration.name_range))
                .collect();
            assert_eq!(
                names,
                ["first", "Box", "öpen", "s", "top"],
                "separator {separator:?}"
            );
            let box_range = file.declarations[1].range;
            assert!(slice(&text, box_range).starts_with("class Box"));
            assert_eq!(file.declarations[2].parent, Some(1));
            assert_eq!(
                file.declaration_at(file.declarations[2].name_range.start),
                Some(2)
            );
        }
    }

    #[test]
    fn mismatched_path_and_text_counts_are_refused() {
        let inputs = [SourceInput::kotlin("class A")];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            0,
            1,
            PlatformProvider::jvm(Box::new(EmptySymbolSource)),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert_eq!(
            analysis_round(&analysis, &[], &["class A"]).unwrap_err(),
            "The Analysis API needs the path and text of every source: 0 paths and 1 texts were \
             given for 1 sources"
        );
    }
}
