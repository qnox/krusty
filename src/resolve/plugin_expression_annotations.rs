//! Classifier annotations exposed to post-resolution plugin expression planning.
//!
//! Source declarations already carry finalized annotation identities. A classifier named by a
//! selected call may instead come from a normalized dependency provider, where Kotlin metadata is
//! authoritative. This boundary combines those two stores before plugins run; plugins never retry
//! resolution or query a classpath themselves.

use std::collections::{HashMap, HashSet};

use crate::libraries::SemanticPlatform;
use crate::plugins::FrontendSelectedCall;
use crate::types::{Ty, TypeName};

use super::SymbolTable;

pub(super) struct ClassifierAnnotationInputs<'a> {
    pub(super) resolved_index: Option<&'a crate::fir::ResolvedModuleIndex>,
    pub(super) pass_one_symbols: Option<&'a SymbolTable>,
    pub(super) libraries: &'a dyn SemanticPlatform,
}

pub(super) fn classifier_annotations_for_calls(
    inputs: ClassifierAnnotationInputs<'_>,
    calls: &[FrontendSelectedCall],
) -> HashMap<TypeName, Vec<TypeName>> {
    let ClassifierAnnotationInputs {
        resolved_index,
        pass_one_symbols,
        libraries,
    } = inputs;
    let mut annotations: HashMap<TypeName, Vec<TypeName>> = if let Some(index) = resolved_index {
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
        pass_one_symbols
            .expect("legacy analysis requires collected source symbols")
            .classes
            .iter()
            .map(|(&classifier, class)| (classifier, class.annotations.clone()))
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_checker_reads_collected_classifier_annotation_identities() {
        let source = "package sample\nannotation class Mark\n@Mark class Target";
        let mut diagnostics = crate::diag::DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = crate::parser::parse(source, &tokens, &mut diagnostics);
        let symbols = super::super::collect_signatures(&[file], &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

        let annotations = classifier_annotations_for_calls(
            ClassifierAnnotationInputs {
                resolved_index: None,
                pass_one_symbols: Some(&symbols),
                libraries: &crate::libraries::EmptySymbolSource,
            },
            &[],
        );
        assert_eq!(
            annotations.get(&crate::types::type_name("sample/Target")),
            Some(&vec![crate::types::type_name("sample/Mark")])
        );
    }
}
