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

#[derive(Clone, Copy)]
pub(super) struct ClassifierAnnotationInputs<'a> {
    pub(super) resolved_index: Option<&'a crate::fir::ResolvedModuleIndex>,
    pub(super) pass_one_symbols: Option<&'a SymbolTable>,
    pub(super) libraries: &'a dyn SemanticPlatform,
}

/// Every classifier a selected call names. A call can name a classpath type in an
/// inferred/explicit type argument, parameter, receiver, or result. Walk each semantic type
/// recursively so `Container<Dependency>` makes both identities available without a
/// classpath-wide scan.
pub(super) fn named_classifiers(calls: &[FrontendSelectedCall]) -> HashSet<TypeName> {
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
    named
}

/// The classifier annotations post-resolution planning reads for one checked body.
///
/// Source classifiers are answered per query from the module's finalized declarations; only the
/// dependency classifiers the calls name are collected eagerly. Planning runs once per checked body,
/// so materializing every source classifier here would make a module's planning quadratic in its
/// size.
pub(super) struct PlanningClassifierAnnotations<'a> {
    source: SourceClassifierAnnotations<'a>,
    dependencies: HashMap<TypeName, Vec<TypeName>>,
}

#[derive(Clone, Copy)]
enum SourceClassifierAnnotations<'a> {
    Resolved(&'a crate::fir::ResolvedModuleIndex),
    Collected(&'a SymbolTable),
}

impl<'a> SourceClassifierAnnotations<'a> {
    fn annotations(self, classifier: TypeName) -> Option<&'a [TypeName]> {
        match self {
            Self::Resolved(index) => {
                let declaration = index.classifier_declaration(classifier)?;
                index.classifier_header(declaration)?;
                Some(index.declaration_annotations(declaration))
            }
            Self::Collected(symbols) => symbols
                .classes
                .get(&classifier)
                .map(|class| class.annotations.as_slice()),
        }
    }
}

impl crate::plugins::FrontendClassifierAnnotations for PlanningClassifierAnnotations<'_> {
    fn classifier_annotations(&self, classifier: TypeName) -> Option<&[TypeName]> {
        self.source
            .annotations(classifier)
            .or_else(|| self.dependencies.get(&classifier).map(Vec::as_slice))
    }
}

pub(super) fn classifier_annotations_for_calls<'a>(
    inputs: ClassifierAnnotationInputs<'a>,
    calls: &[FrontendSelectedCall],
) -> PlanningClassifierAnnotations<'a> {
    let ClassifierAnnotationInputs {
        resolved_index,
        pass_one_symbols,
        libraries,
    } = inputs;
    let source = match resolved_index {
        Some(index) => SourceClassifierAnnotations::Resolved(index),
        None => SourceClassifierAnnotations::Collected(
            pass_one_symbols.expect("legacy analysis requires collected source symbols"),
        ),
    };
    let dependencies = named_classifiers(calls)
        .into_iter()
        .filter(|&classifier| source.annotations(classifier).is_none())
        .filter_map(|classifier| {
            let shape = crate::symbol_source::SymbolSource::classifier(libraries, classifier)?;
            Some((
                classifier,
                shape
                    .annotations
                    .iter()
                    .map(|annotation| annotation.annotation)
                    .collect(),
            ))
        })
        .collect();
    PlanningClassifierAnnotations {
        source,
        dependencies,
    }
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
            crate::plugins::FrontendClassifierAnnotations::classifier_annotations(
                &annotations,
                crate::types::type_name("sample/Target"),
            ),
            Some([crate::types::type_name("sample/Mark")].as_slice())
        );
    }
}
