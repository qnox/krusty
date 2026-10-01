//! The classifier type named by a callable-reference or class-literal LHS (`Outer<A>.Inner<B>::m`),
//! with the applied type preserved at every nested edge.

use super::*;

impl Checker<'_> {
    /// Resolve the classifier chain forming a callable-reference LHS and preserve the applied type
    /// at every nested edge. Unlike the general qualifier probe, this understands `::` as an inner
    /// classifier separator, but only while the enclosing callable-reference/class-literal syntax
    /// owns the interpretation.
    pub(super) fn callable_ref_lhs_classifier_type(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        diagnostic_site: ExprId,
    ) -> Result<Option<(TypeName, Ty)>, QualifierError> {
        let mut segments = Vec::new();
        callable_ref_lhs_classifier_segments(self.file, expression, &mut segments)?;
        let Some((root_expression, root_name)) = segments.first() else {
            return Err(QualifierError::NotANameChain { expression });
        };
        if self.qualifier_root_is_value(scope, root_name) {
            return Ok(None);
        }

        let (root_selection, _, root_alias) = self.select_classifier_binding(scope, root_name);
        let mut prefix = match root_selection {
            InheritedNestedClassifier::Found(classifier) => {
                ResolvedQualifier::Classifier(classifier)
            }
            InheritedNestedClassifier::Ambiguous => {
                return Err(QualifierError::AmbiguousRoot {
                    expression: Some(*root_expression),
                    name: root_name.clone(),
                });
            }
            InheritedNestedClassifier::NotFound => {
                if let Some(classifier) = self.scoped_source_alias_classifier(scope, root_name) {
                    root_alias = true;
                    ResolvedQualifier::Classifier(classifier)
                } else if self.fed_source().package_exists(TypeName::ROOT, root_name) {
                    ResolvedQualifier::Package(crate::types::type_name_child(
                        TypeName::ROOT,
                        root_name,
                    ))
                } else {
                    return Err(QualifierError::UnresolvedSegment {
                        expression: Some(*root_expression),
                        name: root_name.clone(),
                    });
                }
            }
        };

        let mut applied_parent = match (prefix, root_alias) {
            (ResolvedQualifier::Classifier(_), Some(alias))
                if self.has_type_arguments(*root_expression) =>
            {
                crate::trace_compiler!(
                    "resolve",
                    "selected callable-reference typealias identity={:?} spelling={root_name}",
                    alias.identity,
                );
                let arguments = self
                    .file
                    .call_type_args
                    .get(&root_expression.0)
                    .cloned()
                    .unwrap_or_default();
                let span = self.file.expr_spans[root_expression.0 as usize];
                Some(self.alias_application_ty(
                    scope,
                    alias.formals,
                    alias.expansion,
                    root_name,
                    &arguments,
                    span,
                ))
            }
            (ResolvedQualifier::Classifier(internal), _) => {
                Some(self.applied_callable_ref_classifier(
                    scope,
                    *root_expression,
                    diagnostic_site,
                    internal,
                    None,
                ))
            }
            (ResolvedQualifier::Package(_) | ResolvedQualifier::Value, _) => None,
        };

        for (segment_expression, segment) in segments.iter().skip(1) {
            prefix = match prefix {
                ResolvedQualifier::Value => return Ok(None),
                ResolvedQualifier::Package(package) => {
                    if let Some(classifier) = classifier_identity(
                        &self.fed_source(),
                        crate::symbol_source::SymbolNamespace::Package(package),
                        segment,
                    ) {
                        ResolvedQualifier::Classifier(classifier)
                    } else if self.fed_source().package_exists(package, segment) {
                        ResolvedQualifier::Package(crate::types::type_name_child(package, segment))
                    } else {
                        return Err(QualifierError::UnresolvedSegment {
                            expression: Some(*segment_expression),
                            name: segment.clone(),
                        });
                    }
                }
                ResolvedQualifier::Classifier(owner) => {
                    let Some(classifier) = classifier_identity(
                        &self.fed_source(),
                        crate::symbol_source::SymbolNamespace::Classifier(owner),
                        segment,
                    ) else {
                        return Err(QualifierError::UnresolvedSegment {
                            expression: Some(*segment_expression),
                            name: segment.clone(),
                        });
                    };
                    ResolvedQualifier::Classifier(classifier)
                }
            };
            applied_parent = match prefix {
                ResolvedQualifier::Classifier(internal) => {
                    Some(self.applied_callable_ref_classifier(
                        scope,
                        *segment_expression,
                        diagnostic_site,
                        internal,
                        applied_parent,
                    ))
                }
                ResolvedQualifier::Package(_) | ResolvedQualifier::Value => None,
            };
        }

        match (prefix, applied_parent) {
            (ResolvedQualifier::Classifier(internal), Some(applied)) => {
                Ok(Some((internal, applied)))
            }
            (ResolvedQualifier::Package(_), _) => Err(QualifierError::UnresolvedSegment {
                expression: Some(expression),
                name: segments
                    .last()
                    .map(|(_, name)| name.clone())
                    .unwrap_or_default(),
            }),
            (ResolvedQualifier::Value, _) => Ok(None),
            (ResolvedQualifier::Classifier(_), None) => unreachable!("classifier has applied type"),
        }
    }

    fn applied_callable_ref_classifier(
        &mut self,
        scope: &CheckerScope<'_>,
        segment_expression: ExprId,
        diagnostic_site: ExprId,
        internal: TypeName,
        bound_outer: Option<Ty>,
    ) -> Ty {
        let arguments = self
            .file
            .call_type_args
            .get(&segment_expression.0)
            .cloned()
            .unwrap_or_default();
        let applied = if arguments.is_empty() {
            Ty::obj_name(internal)
        } else {
            self.classifier_type_with_arguments(scope, internal, &arguments)
        };
        if applied == Ty::Error {
            return Ty::Error;
        }
        let Some(classifier) = self.resolver().classifier(internal) else {
            return applied;
        };
        if classifier.own_type_parameter_count == classifier.type_params().len() {
            return applied;
        }

        // A raw inner classifier token/reference (`Outer.Inner::class`) names classifier identity;
        // it does not construct `Inner` and therefore needs neither an enclosing value nor applied
        // outer arguments. Preserve the raw type when every segment is unapplied. An applied parent
        // (`Outer<T>.Inner::class`) still supplies exact captures through the path below.
        if arguments.is_empty() && bound_outer.is_some_and(|outer| outer.type_args().is_empty()) {
            return applied;
        }

        // `classifier_type_with_arguments` fills captured positions with declaration formals for an
        // unqualified type use. A callable-reference LHS has the actual applied parent in hand, so
        // retain only this segment's own arguments and bind captures from that parent (or from the
        // lexical scope for a root inner/local classifier).
        let own_count = classifier.own_type_parameter_count;
        let own_arguments = applied
            .type_args()
            .iter()
            .copied()
            .take(own_count)
            .collect::<Vec<_>>();
        let own_instance = if own_arguments.is_empty() {
            Ty::obj_name(internal)
        } else {
            Ty::obj_args_name(internal, &own_arguments)
        };
        crate::trace_compiler!(
            "constructor_ref",
            "applied callable-reference classifier={internal:?} segment={segment_expression:?} applied={applied:?} own_instance={own_instance:?} bound_outer={bound_outer:?}",
        );
        self.attach_captured_classifier_arguments(
            scope,
            diagnostic_site,
            internal,
            own_instance,
            bound_outer,
        )
    }

    pub(super) fn has_type_arguments(&self, expression: ExprId) -> bool {
        self.file
            .call_type_args
            .get(&expression.0)
            .is_some_and(|arguments| !arguments.is_empty())
    }
}
