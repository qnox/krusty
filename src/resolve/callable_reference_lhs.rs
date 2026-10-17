//! The classifier type named by a callable-reference or class-literal LHS (`Outer<A>.Inner<B>::m`),
//! with the applied type preserved at every nested edge.

use super::*;

impl Checker<'_> {
    /// Resolve a bare unbound class-literal receiver through the ordinary type-reference channel,
    /// including parser-owned type arguments. Values remain bound literals and type parameters use
    /// the reified path.
    pub(super) fn class_literal_unbound_ty(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        name: &str,
    ) -> Option<Ty> {
        if scope.tparam_contains(name) || self.lookup(scope, name).is_some() {
            return None;
        }
        let explicit_arguments = self
            .file
            .call_type_args
            .get(&expression.0)
            .is_some_and(|arguments| !arguments.is_empty());
        if self.file.bare_array_class_literal && !explicit_arguments {
            let internal = self.select_classifier(scope, name).found()?;
            // A parameterless typealias may expand to a fully applied reference-array type. Keep
            // that expansion intact; only the raw classifier selected directly from this scope
            // receives Kotlin's implicit star projection for a bare class literal.
            let source_alias = scope.type_alias(name).is_some()
                || self
                    .scoped_source_alias_identity(scope, name)
                    .and_then(|identity| self.source_alias_expansion(identity))
                    .is_some()
                || self
                    .selected_alias_expansion(name, internal, false)
                    .is_some();
            if Ty::obj_name(internal).is_reference_array() && !source_alias {
                return Some(Ty::obj_args_name(
                    internal,
                    &[Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
                ));
            }
        }
        let reference = TypeRef {
            name: name.to_string(),
            flags: TrFlags::default().with_class_literal_lhs(true),
            arg: None,
            targs: self
                .file
                .call_type_args
                .get(&expression.0)
                .map(|syntax| syntax.arguments.clone())
                .unwrap_or_default(),
            span: self.file.expr_spans[expression.0 as usize],
            fun_params: Vec::new(),
            fun_context_count: 0,
            qualifier_arguments: Vec::new(),
        };
        let ty = self.type_ref_ty(scope, &reference);
        if ty == Ty::Error {
            return None;
        }
        // Keep the Kotlin classifier type in semantic results (`KClass<Int>`, not
        // `KClass<java.lang.Integer>`). The backend chooses the boxed JVM class token when it emits
        // the literal; representation must not leak into frontend generic arguments.
        Some(ty)
    }

    pub(super) fn validate_class_literal_type_arguments(
        &mut self,
        expression: ExprId,
        target: ExprId,
        ty: Ty,
    ) -> Ty {
        let has_type_arguments = self
            .file
            .call_type_args
            .get(&target.0)
            .is_some_and(|arguments| !arguments.is_empty());
        // Written nullable classifier syntax (`A?::class`) is normalized to `A` by the callable-
        // reference parser. A nullable type here therefore comes from semantic alias expansion
        // (`typealias Maybe<T> = Box<T>?`), which does not denote a class-literal left side.
        if ty.is_nullable() {
            self.diags.error(
                self.span(expression),
                "type in a class literal cannot be nullable.".to_string(),
            );
            Ty::Error
        } else if has_type_arguments && !ty.is_reference_array() {
            self.diags.error(
                self.span(expression),
                "only classes are allowed on the left-hand side of a class literal.".to_string(),
            );
            Ty::Error
        } else {
            ty
        }
    }

    /// Resolve the classifier chain forming a callable-reference LHS and preserve the applied type
    /// at every nested edge. Unlike the general qualifier probe, this understands `::` as an inner
    /// classifier separator, but only while the enclosing callable-reference/class-literal syntax
    /// owns the interpretation.
    pub(super) fn callable_ref_lhs_classifier_type(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        diagnostic_site: ExprId,
    ) -> Result<Option<(TypeName, Ty, bool)>, QualifierError> {
        let mut segments = Vec::new();
        callable_ref_lhs_classifier_segments(self.file, expression, &mut segments)?;
        let Some((root_expression, root_name)) = segments.first() else {
            return Err(QualifierError::NotANameChain { expression });
        };
        if self.qualifier_root_is_value(scope, root_name) {
            return Ok(None);
        }
        let class_literal = matches!(
            self.file.expr(diagnostic_site),
            Expr::CallableRef { name, .. } if name == "class"
        );

        let (root_selection, _, root_alias, _) = self.select_classifier_binding(scope, root_name);
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
                if self.fed_source().package_exists(TypeName::ROOT, root_name) {
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

        let (mut applied_parent, mut final_class_literal_arguments_checked) =
            match (prefix, root_alias) {
                (ResolvedQualifier::Classifier(internal), alias) => {
                    let (applied, checked) = self.applied_callable_ref_classifier(
                        scope,
                        *root_expression,
                        diagnostic_site,
                        internal,
                        root_name,
                        alias,
                        None,
                        class_literal,
                    );
                    (Some(applied), checked)
                }
                (ResolvedQualifier::Package(_) | ResolvedQualifier::Value, _) => (None, false),
            };

        for (segment_expression, segment) in segments.iter().skip(1) {
            let (next_prefix, selected_alias) = match prefix {
                ResolvedQualifier::Value => return Ok(None),
                ResolvedQualifier::Package(package) => {
                    let source = self.fed_source();
                    let record = source.symbols(
                        crate::symbol_source::SymbolNamespace::Package(package),
                        segment,
                    );
                    if let Some(classifier) = record.classifier_name {
                        let classifier = self.libraries.canonical_source_type_name(classifier);
                        let alias = self
                            .selected_provider_alias(
                                classifier,
                                record.classifier_declaration.as_ref(),
                            )
                            .map_err(|()| QualifierError::AmbiguousRoot {
                                expression: Some(*segment_expression),
                                name: segment.clone(),
                            })?;
                        (ResolvedQualifier::Classifier(classifier), alias)
                    } else if source.package_exists(package, segment) {
                        (
                            ResolvedQualifier::Package(crate::types::type_name_child(
                                package, segment,
                            )),
                            None,
                        )
                    } else {
                        return Err(QualifierError::UnresolvedSegment {
                            expression: Some(*segment_expression),
                            name: segment.clone(),
                        });
                    }
                }
                ResolvedQualifier::Classifier(owner) => {
                    let source = self.fed_source();
                    let record = source.symbols(
                        crate::symbol_source::SymbolNamespace::Classifier(owner),
                        segment,
                    );
                    let Some(classifier) = record.classifier_name else {
                        return Err(QualifierError::UnresolvedSegment {
                            expression: Some(*segment_expression),
                            name: segment.clone(),
                        });
                    };
                    let classifier = self.libraries.canonical_source_type_name(classifier);
                    let alias = self
                        .selected_provider_alias(classifier, record.classifier_declaration.as_ref())
                        .map_err(|()| QualifierError::AmbiguousRoot {
                            expression: Some(*segment_expression),
                            name: segment.clone(),
                        })?;
                    (ResolvedQualifier::Classifier(classifier), alias)
                }
            };
            prefix = next_prefix;
            let (next_applied, checked) = match prefix {
                ResolvedQualifier::Classifier(internal) => {
                    let (applied, checked) = self.applied_callable_ref_classifier(
                        scope,
                        *segment_expression,
                        diagnostic_site,
                        internal,
                        segment,
                        selected_alias,
                        applied_parent,
                        class_literal,
                    );
                    (Some(applied), checked)
                }
                ResolvedQualifier::Package(_) | ResolvedQualifier::Value => (None, false),
            };
            applied_parent = next_applied;
            final_class_literal_arguments_checked = checked;
        }

        match (prefix, applied_parent) {
            (ResolvedQualifier::Classifier(internal), Some(applied)) => Ok(Some((
                internal,
                applied,
                final_class_literal_arguments_checked,
            ))),
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
        display_name: &str,
        alias: Option<super::classifier_binding::SelectedTypeAlias>,
        bound_outer: Option<Ty>,
        class_literal: bool,
    ) -> (Ty, bool) {
        let arguments = self
            .file
            .call_type_args
            .get(&segment_expression.0)
            .map(|syntax| syntax.arguments.clone())
            .unwrap_or_default();
        let alias_was_applied = alias.is_some();
        let (applied, class_literal_arguments_checked) = if let Some(alias) = alias {
            crate::trace_compiler!(
                "resolve",
                "selected callable-reference typealias identity={:?} target={internal:?}",
                alias.identity,
            );
            if class_literal && !arguments.is_empty() {
                (
                    self.check_class_literal_alias_type_arguments(
                        scope,
                        diagnostic_site,
                        segment_expression,
                        display_name,
                        alias,
                    ),
                    true,
                )
            } else if !class_literal && arguments.is_empty() {
                // An ordinary callable reference on an object typealias does not apply the
                // alias's unused type parameters. It selects the already-normalized object target;
                // only a class literal gives omitted parameters generated star arguments.
                (Ty::obj_name(internal), false)
            } else {
                let alias_arguments = if class_literal && arguments.is_empty() {
                    super::alias_projection::AliasArguments::StarProjected
                } else {
                    super::alias_projection::AliasArguments::Written(&arguments)
                };
                let span = self.file.expr_spans[segment_expression.0 as usize];
                let applied = self.alias_application_ty(
                    scope,
                    alias.formals,
                    alias.expansion,
                    display_name,
                    alias_arguments,
                    span,
                );
                self.report_alias_projection_conflicts(&applied.conflicts, applied.ty);
                (applied.ty, false)
            }
        } else if arguments.is_empty() {
            (Ty::obj_name(internal), false)
        } else {
            let applied = self.classifier_type_with_arguments(scope, internal, &arguments);
            self.check_written_application_bounds(&arguments, applied);
            (applied, false)
        };
        if applied == Ty::Error {
            return (Ty::Error, class_literal_arguments_checked);
        }
        // Alias application has already produced the complete expanded type, including captured
        // arguments of an inner target. Rebuilding that type from this segment and `bound_outer`
        // would discard captures when a package-qualified alias has no syntactic parent classifier.
        if alias_was_applied {
            return (applied, class_literal_arguments_checked);
        }
        let Some(classifier) = self.resolver().classifier(internal) else {
            return (applied, class_literal_arguments_checked);
        };
        if classifier.own_type_parameter_count == classifier.type_params().len() {
            return (applied, class_literal_arguments_checked);
        }

        // A raw inner classifier token/reference (`Outer.Inner::class`) names classifier identity;
        // it does not construct `Inner` and therefore needs neither an enclosing value nor applied
        // outer arguments. Preserve the raw type when every segment is unapplied. An applied parent
        // (`Outer<T>.Inner::class`) still supplies exact captures through the path below.
        if arguments.is_empty() && bound_outer.is_some_and(|outer| outer.type_args().is_empty()) {
            return (applied, class_literal_arguments_checked);
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
        (
            self.attach_captured_classifier_arguments(
                scope,
                diagnostic_site,
                internal,
                own_instance,
                bound_outer,
            ),
            class_literal_arguments_checked,
        )
    }

    pub(super) fn has_type_arguments(&self, expression: ExprId) -> bool {
        self.file
            .call_type_args
            .get(&expression.0)
            .is_some_and(|arguments| !arguments.is_empty())
    }
}
