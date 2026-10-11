//! Deprecation reports on import directives.
//!
//! kotlinc never reports the declaration an import names, only the classes the import path goes
//! through: the container of the imported declaration (the class owning an imported classifier,
//! member or wildcard) reports at its declared level, and each class enclosing that container
//! reports through the `ReportDeprecationsOfOuterImportedClasses` migration. Reports start at the
//! import path, innermost class first. They need the published deprecation of source classes, so
//! they run once per file after Pass 1 has published checked declaration annotations.

use super::*;

/// The classifiers an import path names, outermost first, and whether its last segment is one of
/// them. `None` when a qualifier segment does not resolve; the import check reports that.
struct ImportedClassifiers {
    classifiers: Vec<TypeName>,
    imports_classifier: bool,
}

fn imported_classifiers(
    import: &crate::ast::ImportPath,
    source: &dyn SymbolSource,
) -> Option<ImportedClassifiers> {
    use crate::symbol_source::SymbolNamespace;
    let mut package = TypeName::ROOT;
    let mut classifiers = Vec::new();
    let mut imports_classifier = false;
    for (index, (segment, _)) in import.segments.iter().enumerate() {
        let terminal = index + 1 == import.segments.len() && !import.wildcard;
        let namespace = match classifiers.last() {
            Some(&owner) => SymbolNamespace::Classifier(owner),
            None => SymbolNamespace::Package(package),
        };
        if let Some(classifier) = source.symbols(namespace, segment).classifier_name {
            classifiers.push(classifier);
            imports_classifier = terminal;
        } else if classifiers.is_empty() && source.package_exists(package, segment) {
            package = crate::types::type_name_child(package, segment);
        } else if terminal {
            break;
        } else {
            return None;
        }
    }
    Some(ImportedClassifiers {
        classifiers,
        imports_classifier,
    })
}

/// Report the deprecated classes every source file's imports go through.
pub(crate) fn check_import_deprecations(
    files: &[File],
    table: &SymbolTable,
    index: &crate::fir::ResolvedModuleIndex,
    diags: &mut DiagSink,
) {
    for (file_index, file) in files.iter().enumerate() {
        if file.import_paths.is_empty() {
            continue;
        }
        let file_index = u32::try_from(file_index).expect("too many source files");
        let module = crate::module_symbols::ModuleSymbols::for_file(table, file_index);
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn SymbolSource,
            table.libraries.as_ref() as &dyn SymbolSource,
        ]);
        let imports = file
            .import_paths
            .iter()
            .filter_map(|import| {
                let (first, last) = (import.segments.first()?.1, import.segments.last()?.1);
                let imported = imported_classifiers(import, &source)?;
                // The container of the imported declaration, then each class enclosing it.
                let container = if imported.imports_classifier {
                    imported.classifiers.len().checked_sub(2)?
                } else {
                    imported.classifiers.len().checked_sub(1)?
                };
                Some((
                    Span::new(first.lo, last.hi),
                    imported.classifiers[..=container].to_vec(),
                ))
            })
            .collect::<Vec<_>>();
        if imports.is_empty() {
            continue;
        }
        diags.set_file(file_index);
        let mut checker = make_checker_with_index(
            file,
            file_index,
            Some(files),
            table,
            Some(index),
            None,
            None,
            diags,
        );
        checker.push_file_policies(
            index.file_lexical_policies(crate::fir::SourceFileId::from_raw(file_index)),
        );
        for (span, classes) in imports {
            checker.report_import_deprecations(span, &classes);
        }
    }
}
