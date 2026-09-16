//! Temporary annotation identities for checkers constructed before signature finalization.
//!
//! The common annotation path never chooses a declaration by origin. This bridge exists solely for
//! the public single-file `analyze_source` / `check_file*` APIs, whose legacy checker has no
//! [`crate::fir::ResolvedModuleIndex`] yet. Delete it when those entry points construct their checker
//! from the finalized declaration index like production Pass 2.

use std::collections::HashMap;

use crate::ast::{Decl, File};

use super::{AnnotationRef, CheckerModuleSymbols, SymbolTable, TypeName};

pub(super) fn resolved_annotation(
    module: &CheckerModuleSymbols<'_>,
    file_index: u32,
    annotation: &AnnotationRef,
) -> Option<TypeName> {
    module
        .legacy_symbols()?
        .resolved_annotation(file_index, annotation)
}

pub(super) fn classifier_annotations(
    file: &File,
    file_index: u32,
    source_files: Option<&[File]>,
    active_declarations: Option<&crate::fir::ActiveSourceDeclarations>,
    symbols: &SymbolTable,
) -> HashMap<TypeName, Vec<TypeName>> {
    let source_at = |index: u32| {
        (index == file_index)
            .then_some(file)
            .or_else(|| source_files?.get(index as usize))
    };
    symbols
        .classes
        .iter()
        .filter_map(|(&classifier, class)| {
            let source = source_at(class.source_file)?;
            let declaration = match active_declarations {
                Some(active) if class.source_file == file_index => {
                    active.class(source, class.stable_declaration?)?.1
                }
                Some(_) => return None,
                None => match source.decl(class.source_decl?) {
                    Decl::Class(declaration) => declaration,
                    Decl::Fun(_) | Decl::Property(_) => return None,
                },
            };
            let annotations = declaration
                .annotations
                .iter()
                .filter_map(|annotation| symbols.resolved_annotation(class.source_file, annotation))
                .collect::<Vec<_>>();
            Some((classifier, annotations))
        })
        .collect()
}
