//! Language-defined factory lookup for collection-literal syntax.
//!
//! A collection literal first offers the expected classifier's companion `operator fun of`.
//! Kotlin additionally defines qualified standard-library factories for its built-in collection
//! interfaces. This module owns only that syntax-to-symbol convention; candidate applicability,
//! overload selection, generic inference, and call commitment remain in the ordinary resolver.

use crate::ast::{Expr, ExprId};
use crate::libraries::FunctionInfo;
use crate::symbol_resolver::CallArgKind;
use crate::types::{type_name, Ty, TyKind, TypeName};

use super::Checker;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StandardCollectionFactory {
    pub(super) package: TypeName,
    pub(super) callable: &'static str,
}

/// Return the qualified standard-library factory associated with a semantic classifier identity.
///
/// Arrays are deliberately absent: the checker handles their compiler-synthetic construction
/// before consulting this table. Matching is by interned qualified identity, never source spelling.
pub(super) fn standard_factory(classifier: TypeName) -> Option<StandardCollectionFactory> {
    let (package, callable) = if classifier.matches("kotlin/collections/List") {
        ("kotlin/collections", "listOf")
    } else if classifier.matches("kotlin/collections/MutableList") {
        ("kotlin/collections", "mutableListOf")
    } else if classifier.matches("kotlin/collections/Set") {
        ("kotlin/collections", "setOf")
    } else if classifier.matches("kotlin/collections/MutableSet") {
        ("kotlin/collections", "mutableSetOf")
    } else if classifier.matches("kotlin/sequences/Sequence") {
        ("kotlin/sequences", "sequenceOf")
    } else {
        return None;
    };
    Some(StandardCollectionFactory {
        package: type_name(package),
        callable,
    })
}

/// With no expected classifier Kotlin's collection-literal fallback is an immutable `List`.
pub(super) fn default_factory() -> StandardCollectionFactory {
    StandardCollectionFactory {
        package: type_name("kotlin/collections"),
        callable: "listOf",
    }
}

impl Checker<'_> {
    /// Whether a selected parameter can supply a language-defined construction strategy for a
    /// collection literal. This is intentionally a target-availability query, not a second call
    /// resolver: overload applicability for the factory itself is checked exactly once when the
    /// enclosing callable has won and `check_collection_literal` commits the expression.
    pub(super) fn collection_literal_target_available(&self, expected: Ty) -> bool {
        let expected = expected.non_null();
        expected.array_elem().is_some()
            || expected.kotlin_class_internal().is_some_and(|classifier| {
                self.classifier_constructs_collection_literals(classifier)
            })
    }

    fn classifier_constructs_collection_literals(&self, classifier: TypeName) -> bool {
        if standard_factory(classifier).is_some() {
            return true;
        }
        self.resolver()
            .classifier_call_candidates(classifier, "of")
            .into_iter()
            .flat_map(|(_, candidates)| candidates)
            .chain(
                self.resolver()
                    .classifier_associated_callables(classifier, "of"),
            )
            .any(|candidate| candidate.flags.operator)
    }

    /// Argument kinds for selecting among `candidates` with each collection literal postponed.
    ///
    /// An expectation-free literal reads as `List<T>`, which is not evidence against a candidate
    /// whose parameter builds the literal itself. The kind instead carries the classifiers among
    /// the candidates' parameters that construct literals, so selection admits those parameters
    /// and the selected parameter then becomes the literal's expected type.
    pub(super) fn postponed_collection_literal_kinds(
        &self,
        args: &[ExprId],
        kinds: Vec<CallArgKind>,
        candidates: &[FunctionInfo],
    ) -> Vec<CallArgKind> {
        if !args
            .iter()
            .any(|arg| self.file.collection_literal_calls.contains(&arg.0))
        {
            return kinds;
        }
        let mut factory_classifiers = Vec::new();
        for parameter in candidates
            .iter()
            .flat_map(|candidate| candidate.semantic_signature().params.clone())
        {
            if let Some(classifier) = parameter.non_null().kotlin_class_internal() {
                if !factory_classifiers.contains(&classifier)
                    && self.classifier_constructs_collection_literals(classifier)
                {
                    factory_classifiers.push(classifier);
                }
            }
        }
        let factory_classifiers: std::sync::Arc<[TypeName]> = factory_classifiers.into();
        args.iter()
            .zip(kinds)
            .map(|(arg, kind)| {
                if self.file.collection_literal_calls.contains(&arg.0) {
                    CallArgKind::CollectionLiteral {
                        provisional: kind.ty(),
                        factory_classifiers: factory_classifiers.clone(),
                    }
                } else {
                    kind
                }
            })
            .collect()
    }

    /// A collection literal used as a generic argument is ambiguous when the variable can be
    /// built by more than one `operator fun of`. The expected result and a sibling argument
    /// each contribute a classifier: `Set<*> = select(mutableSetOf(), [{}])` can be either
    /// `MutableSet` or `Set`. An unrelated expected classifier does not compete; the result
    /// check reports that mismatch itself.
    pub(super) fn ambiguous_collection_literal(
        &mut self,
        args: &[ExprId],
        arg_tys: &[Ty],
        expected_result: Option<Ty>,
    ) -> Option<ExprId> {
        let literal = args
            .iter()
            .copied()
            .find(|argument| self.file.collection_literal_calls.contains(&argument.0))?;
        let expected = expected_result.filter(|ty| *ty != Ty::Error)?.non_null();
        let expected_classifier = expected.kotlin_class_internal()?;
        if !self.classifier_constructs_collection_literals(expected_classifier) {
            return None;
        }
        let mut candidates = Vec::new();
        for (argument, ty) in args.iter().copied().zip(arg_tys.iter().copied()) {
            if argument == literal {
                continue;
            }
            let Some(classifier) = ty.non_null().kotlin_class_internal() else {
                continue;
            };
            if classifier == expected_classifier
                || !self.classifier_constructs_collection_literals(classifier)
            {
                continue;
            }
            if !self
                .receiver_is_assignable(Ty::obj_name(classifier), Ty::obj_name(expected_classifier))
            {
                continue;
            }
            if !candidates.contains(&classifier) {
                candidates.push(classifier);
            }
        }
        if candidates.is_empty() {
            return None;
        }
        candidates.push(expected_classifier);
        let mut message =
            "type of collection literal is ambiguous. Multiple candidates declare operator 'of':"
                .to_string();
        for classifier in candidates {
            let Some(shape) = self.resolver().classifier(classifier) else {
                continue;
            };
            message.push('\n');
            message.push_str(
                &super::candidate_display::classifier_access_display_from_shape(classifier, &shape),
            );
        }
        self.diags.error(self.span(literal), message);
        Some(literal)
    }

    /// A star element (`Set<*>`) does not choose an untyped lambda parameter. `{ x -> }` reports
    /// that parameter, and the literal is not a type argument for its call. An ambiguous literal
    /// already has its own diagnostic; this still reports the parameter. A determined literal
    /// (`[{}]`, `{ x: Int -> }`) is left to ordinary inference.
    pub(super) fn report_unsolved_star_collection_literals(
        &mut self,
        args: &[ExprId],
        arg_tys: &mut [Ty],
        expected_result: Option<Ty>,
        ambiguous: Option<ExprId>,
    ) {
        let Some(expected) = expected_result.filter(|ty| *ty != Ty::Error) else {
            return;
        };
        let expected = expected.non_null();
        let Some(element) = expected.type_args().first().copied() else {
            return;
        };
        if !matches!(element.kind(), TyKind::StarProjection(_)) {
            return;
        }
        let Some(classifier) = expected.kotlin_class_internal() else {
            return;
        };
        if !self.classifier_constructs_collection_literals(classifier) {
            return;
        }
        for (argument, actual) in args.iter().copied().zip(arg_tys.iter_mut()) {
            if !self.file.collection_literal_calls.contains(&argument.0) {
                continue;
            }
            let Expr::Call { args: elements, .. } = self.file.expr(argument) else {
                continue;
            };
            let mut untyped = Vec::new();
            for &element_expr in elements {
                let Expr::Lambda { params, .. } = self.file.expr(element_expr) else {
                    continue;
                };
                let declared = self.file.lambda_param_types.get(&element_expr.0);
                let spans = self.file.lambda_param_spans.get(&element_expr.0);
                for (index, parameter) in params.iter().enumerate() {
                    if parameter == "_" {
                        continue;
                    }
                    let explicit = declared
                        .and_then(|types| types.get(index))
                        .is_some_and(Option::is_some);
                    if explicit {
                        continue;
                    }
                    let span = spans
                        .and_then(|spans| spans.get(index))
                        .copied()
                        .unwrap_or_else(|| self.span(element_expr));
                    untyped.push((span, parameter.clone()));
                }
            }
            if untyped.is_empty() {
                continue;
            }
            // The factory was selected from a parameter that this star never determined
            // (`{ x -> }` is not `(Any) -> Unit`). An ambiguous literal already reports that
            // choice; a determined one reports the factory formal, then the parameter.
            if Some(argument) != ambiguous {
                *actual = Ty::Error;
                self.expr_types[argument.0 as usize] = Ty::Error;
                let formals = self
                    .selected_generic_call_signature(argument)
                    .map(|signature| {
                        signature
                            .formals
                            .iter()
                            .copied()
                            .filter(|formal| {
                                crate::types::ty_mentions_param(
                                    signature.ret,
                                    std::slice::from_ref(formal),
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let span = self.call_callee_name_span(argument);
                for formal in formals {
                    self.report_uninferred_formal_at(span, formal.as_str());
                }
            }
            for (span, parameter) in untyped {
                self.diags.error(
                    span,
                    format!(
                        "cannot infer type for value parameter '{parameter}'. Specify it explicitly."
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_factories_are_selected_by_qualified_classifier_identity() {
        assert_eq!(
            standard_factory(type_name("kotlin/collections/List")),
            Some(StandardCollectionFactory {
                package: type_name("kotlin/collections"),
                callable: "listOf",
            })
        );
        assert_eq!(standard_factory(type_name("sample/List")), None);
    }
}
