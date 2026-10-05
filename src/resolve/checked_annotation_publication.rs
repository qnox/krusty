//! Checked declaration-annotation folding and stable cross-file publication.

use super::*;

/// Fold classifier and member annotations while their declaration scopes and expression arenas are
/// live, then retain only named typed values under stable declaration identities. Bodies are
/// deliberately not selected; this is declaration metadata needed across source files before
/// candidate collection and backend facts freeze.
pub(crate) fn publish_checked_classifier_annotations(
    files: &[File],
    index: &mut crate::fir::ResolvedModuleIndex,
    table: &mut SymbolTable,
    diags: &mut DiagSink,
) {
    for (file_index, file) in files.iter().enumerate() {
        let declarations = table
            .classes
            .values()
            .filter(|class| class.source_file == file_index as u32)
            .filter_map(|class| {
                let stable = class.stable_declaration?;
                index.classifier_header(stable)?;
                Some((class.internal, class.source_decl?))
            })
            .filter(|(_, declaration)| {
                matches!(
                    file.decl(*declaration),
                    Decl::Class(class) if classifier_has_resolved_annotations(table, file_index as u32, class)
                )
            })
            .collect::<Vec<_>>();
        let selected = declarations
            .iter()
            .filter_map(|(_, declaration)| match file.decl(*declaration) {
                Decl::Class(class) => Some(class.span),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        if selected.is_empty() {
            continue;
        }
        diags.set_file(file_index as u32);
        let no_bodies = std::collections::HashSet::new();
        let info = check_file_at_impl_mode_with_index(
            file,
            file_index as u32,
            Some(files),
            table,
            None,
            diags,
            CaptureDiscovery::Published,
            Some(&selected),
            Some(&no_bodies),
            None,
            None,
            None,
            SourceFragmentMode::ClassifierAnnotations,
            None,
        );
        let mut checked = Vec::new();
        for &(internal, declaration) in &declarations {
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            let applications = class
                .annotations
                .iter()
                .filter(|annotation| {
                    table
                        .resolved_annotation(file_index as u32, annotation)
                        .is_some()
                })
                .filter_map(|annotation| info.applied_annotation(annotation))
                .map(|annotation| crate::types::ResolvedAnnotation {
                    annotation: annotation.internal,
                    arguments: annotation.values.clone(),
                    facts: annotation.facts,
                })
                .collect::<Vec<_>>();
            let expected = class
                .annotations
                .iter()
                .filter(|annotation| {
                    table
                        .resolved_annotation(file_index as u32, annotation)
                        .is_some()
                })
                .count();
            if applications.len() == expected {
                checked.push((internal, applications));
            } else if !diags.has_errors() {
                diags.error(
                    class.span,
                    "internal error: checked classifier annotations were not fully published",
                );
            }
        }
        for (internal, applications) in checked {
            if let Some(class) = table.classes.get_mut(&internal) {
                class.applied_annotations = applications;
            }
        }
        for &(internal, declaration) in &declarations {
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            if class.kind != crate::ast::ClassKind::Annotation {
                continue;
            }
            if let Some(targets) =
                checked_declared_targets(file, file_index as u32, table, &info, class)
            {
                table.annotation_targets.insert(internal, targets);
                index.publish_annotation_targets(internal, targets);
            }
        }
        for &(internal, declaration) in &declarations {
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            let Some(owner) = table
                .classes
                .get(&internal)
                .and_then(|class| class.stable_declaration)
            else {
                continue;
            };
            for (method_index, method) in class.methods.iter().enumerate() {
                let applications = method
                    .annotations
                    .iter()
                    .filter_map(|annotation| info.applied_annotation(annotation))
                    .map(|annotation| crate::types::ResolvedAnnotation {
                        annotation: annotation.internal,
                        arguments: annotation.values.clone(),
                        facts: annotation.facts,
                    })
                    .collect::<Vec<_>>();
                if applications.is_empty() {
                    continue;
                }
                let Some(method_declaration) =
                    u32::try_from(method_index).ok().and_then(|sibling| {
                        index.owned_declaration(
                            owner,
                            crate::fir::DeclarationKind::Function,
                            sibling,
                        )
                    })
                else {
                    continue;
                };
                index.publish_declaration_applied_annotations(method_declaration, applications);
            }
        }
    }
}

/// The targets an annotation class's checked `@Target` application lists, in declared order; `None`
/// when it declares no `@Target`.
///
/// Like kotlinc's `getAllowedAnnotationTargets`, this reads each argument's SELECTED enum entry and
/// takes the target that entry's declared name denotes. The entry is the one the checker bound
/// through the ordinary scope and import rules, so an aliased or unqualified import names the entry
/// it selects, never its spelling. kotlinc does not filter the entry's enum class: an argument of
/// another enum is an argument type mismatch, and the entry it selected still counts.
fn checked_declared_targets(
    file: &File,
    file_index: u32,
    table: &SymbolTable,
    info: &TypeInfo,
    class: &ClassDecl,
) -> Option<crate::types::AnnotationTargets> {
    let (_, arguments) = class
        .annotations
        .iter()
        .zip(&class.annotation_args)
        .find(|(annotation, _)| {
            table
                .resolved_annotation(file_index, annotation)
                .is_some_and(|name| name == type_name("kotlin/annotation/Target"))
        })?;
    let mut targets = Vec::new();
    for &argument in arguments {
        selected_target_entries(file, info, argument, &mut targets);
    }
    Some(crate::types::AnnotationTargets::kotlin(targets))
}

/// `@Target` takes a `vararg` of entries; the named form spells the same list as an array literal
/// or `arrayOf`, so both flatten here.
fn selected_target_entries(
    file: &File,
    info: &TypeInfo,
    argument: ExprId,
    targets: &mut Vec<crate::types::KotlinTarget>,
) {
    if let Some(entry) = info.resolved_enum_entry(argument) {
        targets.extend(crate::types::KotlinTarget::from_entry(&entry.name));
        return;
    }
    let elements = match file.expr(argument) {
        Expr::AnnotationArrayLiteral(elements) => elements.as_slice(),
        Expr::Call { args, .. } => args.as_slice(),
        _ => return,
    };
    for &element in elements {
        selected_target_entries(file, info, element, targets);
    }
}

fn classifier_has_resolved_annotations(
    table: &SymbolTable,
    file_index: u32,
    class: &ClassDecl,
) -> bool {
    let resolved = |annotation: &crate::ast::AnnotationRef| {
        table.resolved_annotation(file_index, annotation).is_some()
    };
    class.annotations.iter().any(resolved)
        || class
            .methods
            .iter()
            .any(|method| method.annotations.iter().any(resolved))
}

impl Checker<'_> {
    /// `include_value_parameters` is false for the classifier-annotation fragment. That fragment
    /// retains a method's own annotation arguments and releases value-parameter annotation syntax,
    /// so walking those parameters asserts on a complete pass.
    pub(super) fn fold_function_annotation_applications(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
        include_value_parameters: bool,
    ) -> usize {
        let suppression_depth = self.push_declaration_suppressions(
            scope,
            &function.annotations,
            &function.annotation_args,
        );
        self.check_declaration_type_parameter_annotations(scope, function.signature_span.lo);
        for (annotation, arguments) in function.annotations.iter().zip(&function.annotation_args) {
            self.check_annotation_application(scope, annotation, arguments);
        }
        if include_value_parameters {
            for parameter in &function.params {
                for (annotation, arguments) in
                    parameter.annotations.iter().zip(&parameter.annotation_args)
                {
                    self.check_annotation_application(scope, annotation, arguments);
                }
            }
        }
        suppression_depth
    }

    /// Fold the method annotations the classifier-annotation fragment still has. Value-parameter
    /// annotations belong to the method's body unit.
    pub(super) fn check_classifier_method_annotations(
        &mut self,
        class_scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) {
        self.fold_method_annotations_in_temporary_scope(class_scope, function, false);
    }

    pub(super) fn fold_method_annotations_in_temporary_scope(
        &mut self,
        class_scope: &CheckerScope<'_>,
        function: &FunDecl,
        include_value_parameters: bool,
    ) {
        let method_scope = class_scope.child(ScopeKind::Function { receiver: None });
        let type_parameters = method_scope
            .visible_tparams()
            .symbolic_extended_with(
                &function.type_params,
                &function.type_param_bounds,
                &|name| self.select_classifier(&method_scope, name).found(),
            )
            .alpha_renamed_declaration(
                &function.type_params,
                self.compilation_id,
                self.file_index,
                function.signature_span.lo,
            );
        method_scope.declare_tparams(&function.type_params, &type_parameters, |name| {
            function.reified_type_params.contains(name)
        });
        let suppression_depth = self.fold_function_annotation_applications(
            &method_scope,
            function,
            include_value_parameters,
        );
        self.active_statement_suppressions
            .truncate(suppression_depth);
    }

    pub(super) fn fold_annotation_values(
        &mut self,
        internal: TypeName,
        arguments: &[ExprId],
        nested_call: Option<ExprId>,
    ) -> Option<(
        Vec<(String, crate::types::AnnotationValue)>,
        crate::types::AnnotationSemanticFacts,
    )> {
        let (elements, parameters, policy) = self.annotation_shape(internal)?;
        let argument_names = self.annotation_argument_names(arguments, nested_call);
        let parameter_indices = self
            .annotation_argument_parameter_indices(&parameters, arguments, &argument_names)
            .ok()?;
        let mut values = Vec::with_capacity(arguments.len());
        let mut facts = crate::types::AnnotationSemanticFacts::default();
        for ((&argument, index), argument_name) in
            arguments.iter().zip(parameter_indices).zip(&argument_names)
        {
            let (element_name, declared) = elements.get(index)?.clone();
            self.record_annotation_semantic_fact(
                internal,
                &element_name,
                declared,
                argument,
                &mut facts,
            );
            // Same rule as the checker: a vararg element takes its ELEMENT type only when the
            // argument is POSITIONAL. Folding a named one against the element type dropped the
            // expectation inside the array, so a `boolean[]` element fed constants was written
            // with the `I` tag and threw AnnotationTypeMismatchException on read-back.
            let named = argument_name.is_some();
            let expected =
                if parameters.vararg == Some(index) && !named && !self.file.is_spread_arg(argument)
                {
                    declared.array_read_elem()?
                } else {
                    declared
                };
            let value = self.fold_annotation_value(argument, Some(expected))?;
            if parameters.vararg == Some(index) {
                let item_values = match value {
                    crate::types::AnnotationValue::Array(values) => values,
                    value => vec![value],
                };
                if let Some((_, _, crate::types::AnnotationValue::Array(existing))) =
                    values.iter_mut().find(|(slot, _, _)| *slot == index)
                {
                    existing.extend(item_values);
                } else {
                    values.push((
                        index,
                        element_name,
                        crate::types::AnnotationValue::Array(item_values),
                    ));
                }
            } else {
                values.push((index, element_name, value));
            }
        }
        // An omitted vararg element materializes as an empty array only when the DECLARATION is a
        // Kotlin `vararg val`. The Java `value` vararg is synthesized here from an array-typed
        // element, and kotlinc never writes an omitted Java element: emitting `value=[]` would
        // override the `AnnotationDefault` the classfile already carries (`@Dfl()` must keep
        // `value=[x]`, not become `value=[]`).
        if let Some(index) = parameters
            .vararg
            .filter(|_| policy.materialize_omitted_vararg)
        {
            if !values.iter().any(|(slot, _, _)| *slot == index) {
                let (name, _) = elements.get(index)?.clone();
                values.push((
                    index,
                    name,
                    crate::types::AnnotationValue::Array(Vec::new()),
                ));
            }
        }
        values.sort_by_key(|(index, _, _)| *index);
        Some((
            values
                .into_iter()
                .map(|(_, name, value)| (name, value))
                .collect(),
            facts,
        ))
    }

    /// Normalize declaration semantics while the selected argument identity is still available.
    /// The enum spelling is provider input used only to locate the declaration-owned ordinal; the
    /// recorded fact compares resolved classifier and ordinal identities.
    fn record_annotation_semantic_fact(
        &self,
        annotation: TypeName,
        element: &str,
        declared: Ty,
        argument: ExprId,
        facts: &mut crate::types::AnnotationSemanticFacts,
    ) {
        if annotation != type_name("kotlin/Deprecated") || element != "level" {
            return;
        }
        let Some(level) = declared.non_null().obj_internal() else {
            return;
        };
        let Some(hidden_ordinal) = self.classifier_enum_entry_ordinal(level, "HIDDEN") else {
            return;
        };
        facts.deprecated_hidden = self
            .resolved_enum_entries
            .get(&argument)
            .is_some_and(|entry| {
                entry.classifier == level && usize::try_from(entry.ordinal) == Ok(hidden_ordinal)
            });
    }

    /// The targets `internal` declares, whether this module or a library declares the class.
    pub(super) fn declared_annotation_targets(
        &self,
        internal: TypeName,
    ) -> Option<crate::types::AnnotationTargets> {
        if self.module.annotation_retention(internal).is_some() {
            return Some(self.module.annotation_targets(internal));
        }
        Some(
            self.resolver()
                .classifier(internal)?
                .annotation_targets
                .unwrap_or(crate::types::AnnotationTargets::DEFAULT),
        )
    }

    pub(super) fn fold_annotation_application(
        &mut self,
        internal: TypeName,
        arguments: &[ExprId],
    ) -> Option<crate::types::AppliedAnnotation> {
        let (values, mut facts) = self.fold_annotation_values(internal, arguments, None)?;
        facts.optional_expectation = self.is_optional_expectation_classifier(internal);
        let module_retention = self.module.annotation_retention(internal);
        let classifier = self.resolver().classifier(internal)?;
        let retention = module_retention.or_else(|| {
            Some(match classifier.retention.as_deref() {
                Some("SOURCE") => crate::types::AnnotationRetention::Source,
                Some("BINARY" | "CLASS") => crate::types::AnnotationRetention::Binary,
                Some("RUNTIME") => crate::types::AnnotationRetention::Runtime,
                None => crate::types::AnnotationRetention::Default,
                Some(_) => return None,
            })
        })?;
        let targets = self.declared_annotation_targets(internal)?;
        Some(crate::types::AppliedAnnotation {
            internal,
            values,
            facts,
            retention,
            targets,
        })
    }
}
