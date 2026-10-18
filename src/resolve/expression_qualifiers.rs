//! Expression-position qualifier resolution: one left-to-right walk of a dotted name chain, from
//! scope-tower root classification through the symbol-source package and classifier edges.

use super::*;

impl Checker<'_> {
    /// Walk one qualified spelling exactly once, left-to-right. Root classification is scope-tower
    /// work; subsequent package/classifier edges are symbol-source work. A value root commits the
    /// entire spelling to ordinary expression checking, and this walk never performs call/constructor
    /// overload selection or retries another namespace.
    pub(super) fn qualifier_root_is_value(&self, scope: &CheckerScope<'_>, name: &str) -> bool {
        let resolver = self.resolver();
        scope
            .root_value(
                name,
                |receiver| {
                    !crate::symbol_resolver::declared_member_callables(
                        &self.fed_source(),
                        receiver,
                        name,
                    )
                    .properties()
                    .is_empty()
                        || receiver.obj_internal().is_some_and(|owner| {
                            self.fed_source()
                                .classifier(owner)
                                .is_some_and(|classifier| classifier.constants.contains_key(name))
                        })
                },
                || {
                    resolver.top_level_properties(name).iter().any(|property| {
                        property.kind == crate::libraries::PropKind::TopLevel
                            && property.receiver.is_none()
                    })
                },
            )
            .is_some()
    }

    pub(super) fn qualifier(
        &self,
        scope: &CheckerScope<'_>,
        input: QualifierInput<'_>,
    ) -> Result<ResolvedQualifier, QualifierError> {
        let mut segments = Vec::new();
        match input {
            QualifierInput::Expression(expression) => {
                qualifier_expression_segments(self.file, expression, &mut segments)?;
            }
            QualifierInput::Root(name) => {
                segments.push((None, name.to_string()));
            }
        }
        let Some((root_expression, root_name)) = segments.first() else {
            return Err(QualifierError::UnresolvedSegment {
                expression: None,
                name: String::new(),
            });
        };

        let package_barrier =
            package_qualifier_forms::first_segment_that_cannot_name_a_package(self.file, &segments);
        let root_is_value = self.qualifier_root_is_value(scope, root_name);
        crate::trace_compiler!(
            "resolve",
            "qualifier root={root_name} value={root_is_value}",
        );
        if root_is_value {
            return Ok(ResolvedQualifier::Value);
        }

        let classifier_root = match self.select_classifier(scope, root_name) {
            InheritedNestedClassifier::Found(classifier) => Some(classifier),
            InheritedNestedClassifier::Ambiguous => {
                return Err(QualifierError::AmbiguousRoot {
                    expression: *root_expression,
                    name: root_name.clone(),
                });
            }
            InheritedNestedClassifier::NotFound => {
                if let Some(classifier) = self.scoped_source_alias_classifier(scope, root_name) {
                    Some(classifier)
                } else if package_barrier != Some(0)
                    && self.fed_source().package_exists(TypeName::ROOT, root_name)
                {
                    None
                } else {
                    return Err(QualifierError::UnresolvedSegment {
                        expression: *root_expression,
                        name: root_name.clone(),
                    });
                }
            }
        };
        let source = self.fed_source();
        // A classpath enum entry with a body also has a physical nested subclass (`E$ENTRY`). At
        // the source level `E.ENTRY` is nevertheless the enum VALUE, and that value facet wins in
        // expression position before the same segment is considered as a nested classifier. Stop
        // qualifier folding at the first such semantic entry so the ordinary expression checker
        // records the exact enum-entry value and resolves all remaining member segments from it.
        if let Some(mut owner) = classifier_root {
            for (_, segment) in &segments[1..] {
                let is_entry = self.classifier_has_enum_entry(owner, segment);
                if is_entry {
                    return Ok(ResolvedQualifier::Value);
                }
                let Some(next) = classifier_identity(
                    &source,
                    crate::symbol_source::SymbolNamespace::Classifier(owner),
                    segment,
                ) else {
                    break;
                };
                owner = next;
            }
        }
        match (classifier_root, package_barrier) {
            (None, Some(barrier)) => package_qualifier_forms::walk_package_root_with_barrier(
                &source,
                *root_expression,
                root_name,
                &segments,
                barrier,
            ),
            _ => walk_qualifier_namespace_facets(
                &source,
                classifier_root,
                *root_expression,
                root_name,
                &segments[1..],
            ),
        }
    }
}
