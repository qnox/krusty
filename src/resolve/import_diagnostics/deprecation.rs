//! Deprecation checks over import identities retained by import validation.

use super::*;
use crate::features::DeprecationMigration;
use crate::resolve::deprecation_usage::{
    classifier_deprecated_use_by_identity, emit_deprecated_use,
};

pub(crate) fn check_import_deprecations(
    files: &[File],
    inventory: &BoundImportInventory,
    table: &SymbolTable,
    index: &crate::fir::ResolvedModuleIndex,
    diags: &mut DiagSink,
) {
    for (file_index, (file, imports)) in files.iter().zip(&inventory.files).enumerate() {
        let file_index = u32::try_from(file_index).expect("too many source files");
        let policies = index
            .file_lexical_policies(crate::fir::SourceFileId::from_raw(file_index))
            .to_vec();
        diags.set_file(file_index);
        for import in imports {
            let container = if import.imports_classifier {
                import.classifiers.len().checked_sub(2)
            } else {
                import.classifiers.len().checked_sub(1)
            };
            let Some(container) = container else {
                continue;
            };
            let selected = &import.classifiers[..=container];
            let Some((&container, enclosing)) = selected.split_last() else {
                continue;
            };
            emit_deprecated_use(
                diags,
                &file.deprecation_migrations,
                &policies,
                import.span,
                classifier_deprecated_use_by_identity(
                    index,
                    table.libraries.as_ref(),
                    file_index,
                    container,
                ),
                None,
            );
            for &classifier in enclosing.iter().rev() {
                emit_deprecated_use(
                    diags,
                    &file.deprecation_migrations,
                    &policies,
                    import.span,
                    classifier_deprecated_use_by_identity(
                        index,
                        table.libraries.as_ref(),
                        file_index,
                        classifier,
                    ),
                    Some(DeprecationMigration::OuterImportedClass),
                );
            }
        }
    }
}
