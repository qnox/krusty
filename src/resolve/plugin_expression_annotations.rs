//! Classifier annotations exposed to post-resolution plugin expression planning.
//!
//! Source declarations already carry finalized annotation identities. A classifier named by a
//! selected call may instead come from a normalized dependency provider, where Kotlin metadata is
//! authoritative. This boundary combines those two stores before plugins run; plugins never retry
//! resolution or query a classpath themselves.

use std::collections::{HashMap, HashSet};

use crate::ast::File;
use crate::libraries::SemanticPlatform;
use crate::plugins::FrontendSelectedCall;
use crate::types::{Ty, TypeName};

use super::SymbolTable;

pub(super) struct ClassifierAnnotationInputs<'a> {
    pub(super) file: &'a File,
    pub(super) file_index: u32,
    pub(super) source_files: Option<&'a [File]>,
    pub(super) active_declarations: Option<&'a crate::fir::ActiveSourceDeclarations>,
    pub(super) resolved_index: Option<&'a crate::fir::ResolvedModuleIndex>,
    pub(super) pass_one_symbols: Option<&'a SymbolTable>,
    pub(super) libraries: &'a dyn SemanticPlatform,
}

pub(super) fn classifier_annotations_for_calls(
    inputs: ClassifierAnnotationInputs<'_>,
    calls: &[FrontendSelectedCall],
) -> HashMap<TypeName, Vec<TypeName>> {
    let ClassifierAnnotationInputs {
        file,
        file_index,
        source_files,
        active_declarations,
        resolved_index,
        pass_one_symbols,
        libraries,
    } = inputs;
    let mut annotations = if let Some(index) = resolved_index {
        (0..index.declaration_count())
            .filter_map(|raw| {
                let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
                let classifier = index.classifier_header(declaration)?.classifier;
                Some((
                    classifier,
                    index.declaration_annotations(declaration).to_vec(),
                ))
            })
            .collect()
    } else {
        super::annotation_legacy_bridge::classifier_annotations(
            file,
            file_index,
            source_files,
            active_declarations,
            pass_one_symbols.expect("legacy analysis requires collected source symbols"),
        )
    };

    // A selected call can name a classpath type in an inferred/explicit type argument, parameter,
    // receiver, or result. Walk each semantic type recursively so `Container<Dependency>` makes
    // both identities available without a classpath-wide scan.
    let mut named = HashSet::new();
    let mut pending = calls
        .iter()
        .flat_map(|call| {
            call.type_arguments
                .iter()
                .flatten()
                .copied()
                .chain(call.params.iter().copied())
                .chain(std::iter::once(call.ret))
                .chain(call.explicit_receiver.map(|(_, ty)| ty))
        })
        .collect::<Vec<Ty>>();
    while let Some(ty) = pending.pop() {
        if let Some(classifier) = ty.kotlin_class_internal() {
            named.insert(classifier);
        }
        pending.extend(ty.type_args().iter().copied());
    }

    for classifier in named {
        let std::collections::hash_map::Entry::Vacant(entry) = annotations.entry(classifier) else {
            continue;
        };
        let Some(shape) = crate::symbol_source::SymbolSource::classifier(libraries, classifier)
        else {
            continue;
        };
        entry.insert(
            shape
                .annotations
                .iter()
                .map(|annotation| annotation.annotation)
                .collect(),
        );
    }
    annotations
}
