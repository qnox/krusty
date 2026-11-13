//! One open source set analyzed and projected into per-document editor data: the shared symbol
//! indexes are built once for the set, then each file keeps only what editor queries read.

use super::*;

/// Analyze one source in an open source set and retain only data needed by editor queries.
pub fn analyze_for_lsp(sources: &[&str]) -> Vec<DocumentAnalysis> {
    analyze_for_lsp_with_navigation_limit(sources, MAX_SOURCE_SET_NAVIGATION_ENTRIES)
}

/// Register the classes each Java document declares, so Kotlin references to them navigate to the
/// Java source rather than to the classpath stub that made them resolvable.
pub(crate) fn register_java_declarations(
    definition_symbols: &mut DefinitionSymbols,
    sources: &[&str],
    java_documents: &[u32],
) {
    let mut declarations = Vec::new();
    let mut counts = HashMap::new();
    for &index in java_documents {
        let Some(source) = sources.get(index as usize) else {
            continue;
        };
        for declaration in java::global_declared_class_occurrences(source, index) {
            *counts.entry(declaration.0.clone()).or_insert(0usize) += 1;
            declarations.push(declaration);
        }
    }
    for (owner, target) in declarations {
        if counts.get(&owner) == Some(&1)
            && !definition_symbols.class_targets().contains_key(&owner)
        {
            definition_symbols.insert_class_target(owner, target);
        }
    }
}

pub(crate) fn apply_java_navigation(
    analyses: &mut [DocumentAnalysis],
    sources: &[&str],
    java_documents: &[u32],
    definition_symbols: &DefinitionSymbols,
    budgets: &mut AnalysisBudgets,
) {
    let limit = analyses.len().min(sources.len());
    let java_documents = java_documents
        .iter()
        .map(|index| *index as usize)
        .filter(|index| *index < limit)
        .collect::<Vec<_>>();
    if java_documents.is_empty() {
        return;
    }
    for &index in &java_documents {
        let mut targets = definition_symbols.class_targets().clone();
        targets.extend(java::declared_classes(sources[index], index as u32));
        let occurrences = java::definition_occurrences(sources[index], &targets);
        analyses[index].definitions =
            DefinitionIndex::from_occurrences(occurrences, &mut budgets.navigation);
    }
}

pub fn analyze_documents_for_lsp(documents: &[(&str, &str)]) -> Vec<DocumentAnalysis> {
    let inputs = documents
        .iter()
        .map(|(uri, source)| {
            if uri.ends_with(".java") {
                SourceInput::java(source)
            } else {
                SourceInput::kotlin(source)
            }
        })
        .collect::<Vec<_>>();
    analyze_source_inputs_for_lsp(&inputs, MAX_SOURCE_SET_NAVIGATION_ENTRIES)
}

pub(super) fn analyze_for_lsp_with_navigation_limit(
    sources: &[&str],
    navigation_relation_limit: usize,
) -> Vec<DocumentAnalysis> {
    let inputs = sources
        .iter()
        .map(|source| SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    analyze_source_inputs_for_lsp(&inputs, navigation_relation_limit)
}

fn analyze_source_inputs_for_lsp(
    inputs: &[SourceInput<'_>],
    navigation_relation_limit: usize,
) -> Vec<DocumentAnalysis> {
    let analysis = crate::jvm_analysis::analyze_standalone_source_inputs(inputs);
    document_analyses_for_source_set(inputs, analysis, navigation_relation_limit)
}

pub(super) fn document_analyses_for_source_set(
    inputs: &[SourceInput<'_>],
    analysis: crate::compiler_analysis::SourceSetAnalysis,
    navigation_relation_limit: usize,
) -> Vec<DocumentAnalysis> {
    let sources = inputs.iter().map(|input| input.text).collect::<Vec<_>>();
    let java_documents = inputs
        .iter()
        .enumerate()
        .filter(|(_, input)| input.kind == SourceKind::Java)
        .map(|(index, _)| index as u32)
        .collect::<Vec<_>>();
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let mut definition_symbols = DefinitionSymbols::from_source_set(
        &sources,
        &analysis.files,
        &analysis.symbols,
        navigation_relation_limit,
    );
    register_java_declarations(&mut definition_symbols, &sources, &java_documents);
    let completion_symbols = CompletionSymbols::from_source_set(&analysis.files);
    let signature_help_symbols =
        SignatureHelpSymbols::from_source_set(&sources, &analysis.files, &analysis.symbols);
    let workspace_symbols = WorkspaceSymbolIndex::from_source_set(&sources, &analysis.files);
    let indexes = SourceSetIndexes::new(
        &analysis.symbols,
        &highlight_symbols,
        &definition_symbols,
        &completion_symbols,
        &signature_help_symbols,
    );
    let mut budgets = AnalysisBudgets::new();
    let pending = analysis
        .files
        .into_iter()
        .zip(&sources)
        .enumerate()
        .map(|(file_index, (file, source))| {
            DocumentAnalysis::from_file_analysis(
                source,
                file,
                file_index as u32,
                &indexes,
                &mut budgets,
            )
        })
        .collect();
    let mut analyses = finalize_navigation(pending, &mut budgets);
    apply_java_navigation(
        &mut analyses,
        &sources,
        &java_documents,
        &definition_symbols,
        &mut budgets,
    );
    if let Some(first) = analyses.first_mut() {
        first.workspace_symbols = workspace_symbols;
    }
    analyses
}
