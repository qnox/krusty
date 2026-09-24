//! Stable publication of fully checked classifier annotation applications.

use super::*;

/// Fold classifier annotations while their declaration scopes and expression arenas are live, then
/// retain only named typed values under stable declaration identities. Bodies are deliberately not
/// selected; this is declaration metadata needed across source files before backend facts freeze.
pub(crate) fn publish_checked_classifier_annotations(
    files: &[File],
    index: &crate::fir::ResolvedModuleIndex,
    table: &mut SymbolTable,
    diags: &mut DiagSink,
) {
    for (file_index, file) in files.iter().enumerate() {
        let declarations = table
            .classes
            .values()
            .filter(|class| class.source_file == file_index as u32)
            .filter_map(|class| {
                let stable = class.stable_declaration?;
                index.classifier_header(stable)?;
                Some((class.internal, class.source_decl?))
            })
            .filter(|(_, declaration)| {
                matches!(file.decl(*declaration), Decl::Class(class) if class.annotations.iter().any(
                    |annotation| table.resolved_annotation(file_index as u32, annotation).is_some()
                ))
            })
            .collect::<Vec<_>>();
        let selected = declarations
            .iter()
            .filter_map(|(_, declaration)| match file.decl(*declaration) {
                Decl::Class(class) => Some(class.span),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        if selected.is_empty() {
            continue;
        }
        diags.set_file(file_index as u32);
        let no_bodies = std::collections::HashSet::new();
        let info = check_file_at_impl_mode_with_index(
            file,
            file_index as u32,
            Some(files),
            table,
            None,
            diags,
            false,
            Some(&selected),
            Some(&no_bodies),
            None,
            None,
            None,
            SourceFragmentMode::ClassifierAnnotations,
            None,
            None,
        );
        let mut checked = Vec::new();
        for (internal, declaration) in declarations {
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            let applications = class
                .annotations
                .iter()
                .filter(|annotation| {
                    table
                        .resolved_annotation(file_index as u32, annotation)
                        .is_some()
                })
                .filter_map(|annotation| info.applied_annotation(annotation))
                .map(|annotation| crate::types::ResolvedAnnotation {
                    annotation: annotation.internal,
                    arguments: annotation.values.clone(),
                })
                .collect::<Vec<_>>();
            let expected = class
                .annotations
                .iter()
                .filter(|annotation| {
                    table
                        .resolved_annotation(file_index as u32, annotation)
                        .is_some()
                })
                .count();
            if applications.len() == expected {
                checked.push((internal, applications));
            } else if !diags.has_errors() {
                diags.error(
                    class.span,
                    "internal error: checked classifier annotations were not fully published",
                );
            }
        }
        for (internal, applications) in checked {
            if let Some(class) = table.classes.get_mut(&internal) {
                class.applied_annotations = applications;
            }
        }
    }
}
