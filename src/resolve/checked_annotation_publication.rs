//! Checked declaration-annotation folding and stable cross-file publication.

use super::*;
use crate::fir::DeclarationKind;

/// Fold the annotations of classifiers, their members, and top-level callables while their
/// declaration scopes and expression arenas are live, then retain only named typed values under
/// stable declaration identities. Bodies are deliberately not selected; this is declaration
/// metadata needed across source files before candidate collection and backend facts freeze:
/// `@Deprecated` removes a `HIDDEN` callable from candidate collection, and every use of a
/// deprecated declaration reports its level and message.
pub(crate) fn publish_checked_declaration_annotations(
    files: &[File],
    index: &mut crate::fir::ResolvedModuleIndex,
    table: &mut SymbolTable,
    diags: &mut DiagSink,
) {
    let mut constructor_properties = Vec::new();
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
        let top_level = top_level_annotated_declarations(table, file_index as u32, file);
        let selected = declarations
            .iter()
            .filter_map(|(_, declaration)| match file.decl(*declaration) {
                Decl::Class(class) => Some(class.span),
                _ => None,
            })
            .chain(top_level.iter().map(|&(_, _, span)| span))
            .collect::<crate::fx_hash::HashSet<_>>();
        if selected.is_empty() {
            continue;
        }
        diags.set_file(file_index as u32);
        let no_bodies = crate::fx_hash::HashSet::default();
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
            SourceFragmentMode::DeclarationAnnotations,
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
                .map(crate::types::AppliedAnnotation::resolved)
                .collect::<Vec<_>>();
            let fully_checked = class
                .annotations
                .iter()
                .filter(|annotation| {
                    table
                        .resolved_annotation(file_index as u32, annotation)
                        .is_some()
                })
                .all(|annotation| {
                    info.applied_annotation(annotation).is_some()
                        || info.discarded_annotation_application(annotation)
                });
            if fully_checked {
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
            if let Some(retention) = checked_retention(file, file_index as u32, table, &info, class)
            {
                index.publish_annotation_retention(internal, retention);
                table.annotation_retentions.insert(internal, retention);
            }
            let Some(policy) = checked_target_policy(file, file_index as u32, table, &info, class)
            else {
                continue;
            };
            let published = match policy {
                CheckedTargetPolicy::Valid(targets) => {
                    index.publish_annotation_targets(internal, targets, true);
                    table.annotation_targets.publish_valid(internal, targets)
                }
                CheckedTargetPolicy::InvalidRecovery(targets) => {
                    index.publish_annotation_targets(internal, targets, false);
                    table
                        .annotation_targets
                        .publish_invalid_recovery(internal, targets)
                }
            };
            assert!(
                published,
                "an annotation classifier may publish one target policy"
            );
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
            publish_classifier_member_annotations(
                index,
                owner,
                class,
                &info,
                file.property_annotation_default_target_mode,
                &mut constructor_properties,
            );
            if let Some(defaults) = info.annotation_element_defaults(internal) {
                index.publish_annotation_element_defaults(owner, defaults.to_vec());
            }
        }
        let source = crate::fir::SourceFileId::from_raw(file_index as u32);
        for &(ordinal, declaration, _) in &top_level {
            let (kind, annotations) = match file.decl(declaration) {
                Decl::Fun(function) => (DeclarationKind::Function, &function.annotations),
                Decl::Property(property) => (DeclarationKind::Property, &property.annotations),
                Decl::Class(_) => continue,
            };
            let Some(stable) = index
                .source_inventory(source)
                .iter()
                .copied()
                .find(|stable| {
                    index.declaration_anchor(*stable).is_some_and(|anchor| {
                        anchor.owner.is_none() && anchor.kind == kind && anchor.sibling == ordinal
                    })
                })
            else {
                continue;
            };
            index.publish_declaration_applied_annotations(
                stable,
                checked_applications(&info, annotations),
            );
            match file.decl(declaration) {
                Decl::Fun(function) => {
                    publish_parameter_annotations(index, stable, &function.params, &info);
                }
                Decl::Property(property) => {
                    publish_accessor_annotations(index, stable, property, &info);
                }
                Decl::Class(_) => {}
            }
        }
    }
    for property in constructor_properties {
        property.publish(index);
    }
}

/// The non-local top-level functions and properties of `file` whose own, value-parameter or
/// accessor annotations Pass 1 bound, each with its position among the file's declarations, the sibling ordinal of its
/// stable identity.
fn top_level_annotated_declarations(
    table: &SymbolTable,
    file_index: u32,
    file: &File,
) -> Vec<(u32, DeclId, Span)> {
    let resolved = |annotation: &crate::ast::AnnotationRef| {
        table.resolved_annotation(file_index, annotation).is_some()
    };
    file.decls
        .iter()
        .enumerate()
        .filter(|(_, declaration)| !file.is_local_declaration(**declaration))
        .filter_map(|(ordinal, &declaration)| {
            let span = match file.decl(declaration) {
                Decl::Fun(function) if function_annotations(function).any(resolved) => {
                    function.span
                }
                Decl::Property(property) if property_annotations(property).any(&resolved) => {
                    property.span
                }
                _ => return None,
            };
            Some((u32::try_from(ordinal).ok()?, declaration, span))
        })
        .collect()
}

/// A function's own annotations, then those of its value parameters.
fn function_annotations(function: &FunDecl) -> impl Iterator<Item = &crate::ast::AnnotationRef> {
    function.annotations.iter().chain(
        function
            .params
            .iter()
            .flat_map(|parameter| &parameter.annotations),
    )
}

/// A property's own annotations, then those written on its getter and setter.
fn property_annotations(property: &PropDecl) -> impl Iterator<Item = &crate::ast::AnnotationRef> {
    property.annotations.iter().chain(
        property
            .getter_annotations
            .iter()
            .chain(
                property
                    .setter
                    .iter()
                    .flat_map(|setter| &setter.annotations),
            )
            .map(|entry| &entry.annotation),
    )
}

/// The checked applications of `annotations` the fragment folded, in source order.
fn checked_applications<'a>(
    info: &TypeInfo,
    annotations: impl IntoIterator<Item = &'a crate::ast::AnnotationRef>,
) -> Vec<crate::types::ResolvedAnnotation> {
    annotations
        .into_iter()
        .filter_map(|annotation| info.applied_annotation(annotation))
        .map(crate::types::AppliedAnnotation::resolved)
        .collect()
}

/// Retain checked values and declaration policy, not syntax or TypeInfo. Select destinations only
/// after every source annotation classifier has published its target policy.
struct PendingConstructorPropertyAnnotations {
    declaration: crate::fir::DeclarationId,
    host: crate::types::PropertyAnnotationHost,
    mode: crate::types::PropertyAnnotationDefaultTargetMode,
    applications: Vec<(
        Option<crate::types::PropertyAnnotationSite>,
        crate::types::AppliedAnnotation,
    )>,
}

impl PendingConstructorPropertyAnnotations {
    fn publish(self, index: &mut crate::fir::ResolvedModuleIndex) {
        let applications: Vec<_> = self
            .applications
            .into_iter()
            .filter_map(|(explicit_site, annotation)| {
                let targets = index
                    .annotation_targets()
                    .semantic(annotation.internal)
                    .unwrap_or(annotation.targets);
                let sites = targets.property_declaration_sites(self.host, self.mode, explicit_site);
                sites
                    .contains(crate::types::PropertyAnnotationSite::Property)
                    .then(|| annotation.resolved())
            })
            .collect();
        index.publish_declaration_applied_annotations(self.declaration, applications);
    }
}

/// Publish the checked annotations of the members `class` declares under their stable identities
/// owned by `owner`: methods, body properties with their accessors, and constructors.
fn publish_classifier_member_annotations(
    index: &mut crate::fir::ResolvedModuleIndex,
    owner: crate::fir::DeclarationId,
    class: &ClassDecl,
    info: &TypeInfo,
    default_target_mode: crate::types::PropertyAnnotationDefaultTargetMode,
    constructor_properties: &mut Vec<PendingConstructorPropertyAnnotations>,
) {
    for (method_index, method) in class.methods.iter().enumerate() {
        let Some(declaration) = u32::try_from(method_index)
            .ok()
            .and_then(|sibling| index.owned_declaration(owner, DeclarationKind::Function, sibling))
        else {
            continue;
        };
        index.publish_declaration_applied_annotations(
            declaration,
            checked_applications(info, &method.annotations),
        );
        publish_parameter_annotations(index, declaration, &method.params, info);
    }
    for (property_index, property) in class.body_props.iter().enumerate() {
        // A constructor property shares the owner and kind, and numbers its siblings among the
        // constructor parameters.
        let Some(declaration) = u32::try_from(property_index).ok().and_then(|sibling| {
            index
                .owned_declarations(owner)
                .iter()
                .copied()
                .find(|declaration| {
                    index
                        .declaration_anchor(*declaration)
                        .is_some_and(|anchor| {
                            anchor.kind == DeclarationKind::Property && anchor.sibling == sibling
                        })
                        && index
                            .declaration_header(*declaration)
                            .is_some_and(|header| {
                                !header
                                    .flags
                                    .has(crate::fir::DeclarationFlags::PROPERTY_PARAMETER)
                            })
                })
        }) else {
            continue;
        };
        index.publish_declaration_applied_annotations(
            declaration,
            checked_applications(info, &property.annotations),
        );
        publish_accessor_annotations(index, declaration, property, info);
    }
    for (sibling, parameter) in class.props.iter().enumerate() {
        let Some(declaration) = u32::try_from(sibling)
            .ok()
            .filter(|_| parameter.is_property)
            .and_then(|sibling| {
                index
                    .owned_declarations(owner)
                    .iter()
                    .copied()
                    .find(|declaration| {
                        index
                            .declaration_anchor(*declaration)
                            .is_some_and(|anchor| {
                                anchor.kind == DeclarationKind::Property
                                    && anchor.sibling == sibling
                            })
                            && index
                                .declaration_header(*declaration)
                                .is_some_and(|header| {
                                    header
                                        .flags
                                        .has(crate::fir::DeclarationFlags::PROPERTY_PARAMETER)
                                })
                    })
            })
        else {
            continue;
        };
        constructor_properties.push(PendingConstructorPropertyAnnotations {
            declaration,
            host: crate::types::PropertyAnnotationHost {
                constructor_parameter: true,
                has_backing_field: class.kind != crate::ast::ClassKind::Annotation,
            },
            mode: default_target_mode,
            applications: parameter
                .annotations
                .iter()
                .filter_map(|annotation| {
                    Some((
                        annotation.property_site,
                        info.applied_annotation(annotation)?.clone(),
                    ))
                })
                .collect(),
        });
        publish_property_accessor_applications(
            index,
            declaration,
            &parameter.annotations,
            [None, None],
            info,
        );
    }
    // A primary constructor's own annotations are written before `constructor`; one without them
    // still has value parameters whose annotations are published.
    let primary = std::iter::once((
        0,
        class
            .primary_ctor_annotations
            .as_deref()
            .unwrap_or_default(),
        class
            .props
            .iter()
            .map(|parameter| parameter.annotations.as_slice())
            .collect::<Vec<_>>(),
    ));
    let secondary = class
        .secondary_ctors
        .iter()
        .enumerate()
        .map(|(index, constructor)| {
            (
                index + 1,
                constructor.annotations.as_slice(),
                constructor
                    .params
                    .iter()
                    .map(|parameter| parameter.annotations.as_slice())
                    .collect(),
            )
        });
    for (sibling, annotations, parameters) in primary.chain(secondary) {
        let Some(declaration) = u32::try_from(sibling).ok().and_then(|sibling| {
            index.owned_declaration(owner, DeclarationKind::Constructor, sibling)
        }) else {
            continue;
        };
        index.publish_declaration_applied_annotations(
            declaration,
            checked_applications(info, annotations),
        );
        for (position, annotations) in parameters.into_iter().enumerate() {
            let Ok(position) = u32::try_from(position) else {
                continue;
            };
            index.publish_parameter_applied_annotations(
                declaration,
                position,
                checked_applications(info, annotations),
            );
        }
    }
}

/// Publish the checked annotations written on each of `parameters`, the value parameters of the
/// function `declaration`.
fn publish_parameter_annotations(
    index: &mut crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    parameters: &[Param],
    info: &TypeInfo,
) {
    for (position, parameter) in parameters.iter().enumerate() {
        let Ok(position) = u32::try_from(position) else {
            continue;
        };
        index.publish_parameter_applied_annotations(
            declaration,
            position,
            checked_applications(info, &parameter.annotations),
        );
    }
}

/// Publish every application that lands on each accessor of `declaration`, by the property's
/// identity: a default accessor has no declaration of its own, and a use-site target (`@get:`,
/// `@set:`) names the accessor from the property. `written` holds the getter's and the setter's
/// own annotations.
fn publish_property_accessor_applications(
    index: &mut crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    property_annotations: &[crate::ast::AnnotationRef],
    written: [Option<&Vec<crate::ast::AccessorAnnotation>>; 2],
    info: &TypeInfo,
) {
    let [getter, setter] = written;
    for (accessor, site, written) in [
        (
            crate::fir::PropertyAccessor::Getter,
            crate::types::PropertyAnnotationSite::Getter,
            getter,
        ),
        (
            crate::fir::PropertyAccessor::Setter,
            crate::types::PropertyAnnotationSite::Setter,
            setter,
        ),
    ] {
        let targeted = property_annotations
            .iter()
            .filter(|annotation| annotation.property_site == Some(site));
        let written = written.into_iter().flatten().map(|entry| &entry.annotation);
        index.publish_property_accessor_applied_annotations(
            declaration,
            accessor,
            checked_applications(info, targeted.chain(written)),
        );
    }
}

/// Publish the checked annotations written on `property`'s getter and setter under the accessor
/// declarations `declaration` owns.
fn publish_accessor_annotations(
    index: &mut crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    property: &PropDecl,
    info: &TypeInfo,
) {
    let accessors = [
        (0, Some(&property.getter_annotations)),
        (
            1,
            property.setter.as_ref().map(|setter| &setter.annotations),
        ),
    ];
    publish_property_accessor_applications(
        index,
        declaration,
        &property.annotations,
        [accessors[0].1, accessors[1].1],
        info,
    );
    for (sibling, annotations) in accessors {
        let Some(annotations) = annotations.filter(|annotations| !annotations.is_empty()) else {
            continue;
        };
        let Some(accessor) =
            index.owned_declaration(declaration, DeclarationKind::Accessor, sibling)
        else {
            continue;
        };
        index.publish_declaration_applied_annotations(
            accessor,
            checked_applications(info, annotations.iter().map(|entry| &entry.annotation)),
        );
    }
}

/// What an annotation class's own checked `@Target` application declares.
enum CheckedTargetPolicy {
    /// The application checked: its arguments are `kotlin.annotation.AnnotationTarget` entries, and
    /// the targets they denote are the class's semantic policy.
    Valid(crate::types::AnnotationTargets),
    /// The application did not check (an argument of another enum is an argument type mismatch).
    /// kotlinc still lists the entries it selected, by declared name, in the secondary
    /// WRONG_ANNOTATION_TARGET diagnostics; the class has no semantic policy.
    InvalidRecovery(crate::types::AnnotationTargets),
}

/// The class's `@Target` policy from its checked application, in declared order; `None` when it
/// declares no `@Target`. Each argument is the enum entry the checker selected through the ordinary
/// scope and import rules, so an unqualified or aliased import names the entry it selects, never
/// its spelling.
fn checked_target_policy(
    file: &File,
    file_index: u32,
    table: &SymbolTable,
    info: &TypeInfo,
    class: &ClassDecl,
) -> Option<CheckedTargetPolicy> {
    let target = type_name("kotlin/annotation/Target");
    let annotation_target = type_name("kotlin/annotation/AnnotationTarget");
    let is_target = |annotation: &AnnotationRef| {
        table.resolved_annotation(file_index, annotation) == Some(target)
    };
    let index = class.annotations.iter().position(is_target)?;
    let mut leaves = Vec::new();
    for &argument in &class.annotation_args[index] {
        selected_entries(file, info, argument, &mut leaves);
    }
    // Valid only when the application checked and every argument selected an entry declared by
    // `kotlin.annotation.AnnotationTarget` itself.
    let valid = info.applied_annotation(&class.annotations[index]).is_some()
        && leaves
            .iter()
            .all(|leaf| leaf.is_some_and(|entry| entry.classifier == annotation_target));
    let entries = leaves.into_iter().flatten();
    Some(if valid {
        CheckedTargetPolicy::Valid(crate::types::AnnotationTargets::kotlin(entries.filter_map(
            |entry| crate::types::KotlinTarget::of_entry(entry.classifier, &entry.name),
        )))
    } else {
        CheckedTargetPolicy::InvalidRecovery(crate::types::AnnotationTargets::kotlin(
            entries.filter_map(|entry| {
                crate::types::KotlinTarget::recovered_from_invalid_entry(&entry.name)
            }),
        ))
    })
}

/// The retention an annotation class's checked `@Retention` application declares; `None` when it
/// declares none or the application is invalid. The argument is the enum entry the checker selected
/// through the ordinary scope and import rules, so an unqualified or aliased import names the entry
/// it selects, never its spelling. Only an entry declared by `kotlin.annotation.AnnotationRetention`
/// itself is a retention: an entry of another enum, even one spelled the same, is an argument type
/// mismatch, and the class keeps the default.
fn checked_retention(
    file: &File,
    file_index: u32,
    table: &SymbolTable,
    info: &TypeInfo,
    class: &ClassDecl,
) -> Option<crate::types::AnnotationRetention> {
    let retention = type_name("kotlin/annotation/Retention");
    let index = class.annotations.iter().position(|annotation| {
        table.resolved_annotation(file_index, annotation) == Some(retention)
    })?;
    info.applied_annotation(&class.annotations[index])?;
    let mut leaves = Vec::new();
    for &argument in &class.annotation_args[index] {
        selected_entries(file, info, argument, &mut leaves);
    }
    let [Some(entry)] = leaves.as_slice() else {
        return None;
    };
    crate::types::AnnotationRetention::of_entry(entry.classifier, &entry.name)
}

/// Each argument leaf's selected enum entry, `None` for a leaf that selected none. `@Target` takes
/// a `vararg` of entries; the named form spells the same list as an array literal or `arrayOf`, so
/// both flatten here.
fn selected_entries<'i>(
    file: &File,
    info: &'i TypeInfo,
    argument: ExprId,
    leaves: &mut Vec<Option<&'i ResolvedEnumEntry>>,
) {
    if let Some(entry) = info.resolved_enum_entry(argument) {
        leaves.push(Some(entry));
        return;
    }
    let elements = match file.expr(argument) {
        Expr::AnnotationArrayLiteral(elements) => elements.as_slice(),
        Expr::Call { args, .. } => args.as_slice(),
        _ => {
            leaves.push(None);
            return;
        }
    };
    for &element in elements {
        selected_entries(file, info, element, leaves);
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
            .any(|method| function_annotations(method).any(resolved))
        || class
            .body_props
            .iter()
            .any(|property| property_annotations(property).any(&resolved))
        || class
            .primary_ctor_annotations
            .iter()
            .flatten()
            .any(resolved)
        || class
            .props
            .iter()
            .any(|parameter| parameter.annotations.iter().any(resolved))
        || class.secondary_ctors.iter().any(|constructor| {
            constructor
                .annotations
                .iter()
                .chain(
                    constructor
                        .params
                        .iter()
                        .flat_map(|parameter| &parameter.annotations),
                )
                .any(resolved)
        })
        || class.is_annotation()
            && class
                .props
                .iter()
                .any(|parameter| parameter.default.is_some())
}

impl Checker<'_> {
    /// Check a function's own annotations and those of its value parameters.
    pub(super) fn fold_function_annotation_applications(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
        declaration_target: &'static str,
    ) -> usize {
        let suppression_depth =
            self.push_declaration_policies(scope, &function.annotations, &function.annotation_args);
        self.check_declaration_type_parameter_annotations(scope, function.signature_span.lo);
        for (annotation, arguments) in function.annotations.iter().zip(&function.annotation_args) {
            self.check_non_property_annotation_application(
                scope,
                annotation,
                arguments,
                declaration_target,
            );
        }
        for parameter in &function.params {
            for (annotation, arguments) in
                parameter.annotations.iter().zip(&parameter.annotation_args)
            {
                self.check_non_property_annotation_application(
                    scope,
                    annotation,
                    arguments,
                    "value parameter",
                );
            }
        }
        suppression_depth
    }

    /// Whether Pass 1 bound `annotation` to a classifier. The declaration-annotation fragment
    /// folds only those applications; it retains their arguments.
    fn has_bound_annotation(&self, annotation: &crate::ast::AnnotationRef) -> bool {
        self.module
            .legacy_symbols()
            .and_then(|symbols| symbols.resolved_annotation(self.file_index, annotation))
            .is_some()
    }

    /// The source spans of the applications among `annotations` that Pass 1 bound.
    fn bound_annotation_spans(
        &self,
        annotations: &[crate::ast::AnnotationRef],
    ) -> crate::fx_hash::HashSet<crate::diag::Span> {
        annotations
            .iter()
            .filter(|annotation| self.has_bound_annotation(annotation))
            .map(|annotation| annotation.span)
            .collect()
    }

    /// Fold a classifier's own annotations and those of the members it declares, for the
    /// declaration-annotation fragment. Member annotations are declaration facts for later
    /// candidate collection and use-site checks; their declarations' body units check them again,
    /// so a fold this restricted checker cannot complete is not a diagnostic. Value-parameter
    /// annotations belong to the body unit alone.
    pub(super) fn fold_classifier_declaration_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        owner: Option<TypeName>,
    ) {
        for (annotation, arguments) in class.annotations.iter().zip(&class.annotation_args) {
            if self.has_bound_annotation(annotation) {
                self.check_annotation_application(scope, annotation, arguments);
            }
        }
        let diagnostics_before_members = self.diags.diags.len();
        if let Some(owner) = owner.filter(|_| class.is_annotation()) {
            self.fold_annotation_element_defaults(scope, class, owner);
        }
        for method in &class.methods {
            if function_annotations(method).any(|annotation| self.has_bound_annotation(annotation))
            {
                self.fold_method_annotations_in_temporary_scope(scope, method);
            }
        }
        for property in &class.body_props {
            self.fold_property_annotations(
                scope,
                property,
                super::annotation_applications::PropertyAnnotationOwner::Classifier {
                    owner_is_interface: class.is_interface(),
                },
            );
        }
        // A constructor `val`/`var` is a property: fold exactly its Pass-1-bound applications, as
        // a body property's, so publication never binds a name Pass 1 left unbound. A plain
        // constructor parameter's are folded the same way.
        for parameter in &class.props {
            let bound = self.bound_annotation_spans(&parameter.annotations);
            if !bound.is_empty() {
                self.check_selected_constructor_parameter_annotations(
                    scope,
                    parameter,
                    class.is_annotation(),
                    &|annotation| bound.contains(&annotation.span),
                );
            }
        }
        let constructors = class
            .primary_ctor_annotations
            .iter()
            .map(|annotations| (annotations, &class.primary_ctor_annotation_args))
            .chain(
                class
                    .secondary_ctors
                    .iter()
                    .map(|constructor| (&constructor.annotations, &constructor.annotation_args)),
            );
        for (annotations, arguments) in constructors {
            for (annotation, arguments) in annotations.iter().zip(arguments) {
                if self.has_bound_annotation(annotation) {
                    self.check_annotation_application(scope, annotation, arguments);
                }
            }
        }
        for parameter in class
            .secondary_ctors
            .iter()
            .flat_map(|constructor| &constructor.params)
        {
            for (annotation, arguments) in
                parameter.annotations.iter().zip(&parameter.annotation_args)
            {
                if self.has_bound_annotation(annotation) {
                    self.check_non_property_annotation_application(
                        scope,
                        annotation,
                        arguments,
                        "value parameter",
                    );
                }
            }
        }
        self.diags.diags.truncate(diagnostics_before_members);
    }

    /// Fold the default of every element the annotation class `owner` declares in `class`. A
    /// default is a constant like an argument, checked against the element's declared type. The
    /// class's body unit checks the defaults again and reports what is wrong with them, so an
    /// element whose default does not fold leaves the class without published defaults.
    fn fold_annotation_element_defaults(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        owner: TypeName,
    ) {
        let Some((elements, _, _)) = self.annotation_shape(owner) else {
            return;
        };
        if elements.len() != class.props.len()
            || elements
                .iter()
                .zip(&class.props)
                .any(|((name, _), parameter)| *name != parameter.name)
        {
            return;
        }
        let mut defaults = Vec::new();
        for ((name, declared), parameter) in elements.into_iter().zip(&class.props) {
            let Some(default) = parameter.default else {
                continue;
            };
            self.check_annotation_argument_value(scope, default, declared);
            let Some(value) = self.fold_annotation_value(default, Some(declared)) else {
                return;
            };
            defaults.push((name, value));
        }
        self.applied_annotations
            .record_element_defaults(owner, defaults);
    }

    /// Fold a top-level function's or property's annotations for the declaration-annotation
    /// fragment, as [`Self::fold_classifier_declaration_annotations`] folds a member's.
    pub(super) fn fold_top_level_declaration_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
    ) {
        let file = self.file;
        let diagnostics_before = self.diags.diags.len();
        match file.decl(declaration) {
            Decl::Fun(function) => {
                if function_annotations(function)
                    .any(|annotation| self.has_bound_annotation(annotation))
                {
                    self.fold_method_annotations_in_temporary_scope(scope, function);
                }
            }
            Decl::Property(property) => self.fold_property_annotations(
                scope,
                property,
                super::annotation_applications::PropertyAnnotationOwner::File,
            ),
            Decl::Class(_) => {}
        }
        self.diags.diags.truncate(diagnostics_before);
    }

    /// Fold a property's own bound annotations and those written on its accessors. The property's
    /// own applications are checked against the same host as in its body unit, so a use-site
    /// target (`@get:`, `@set:`) is validated for the property that owns it.
    fn fold_property_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
        owner: super::annotation_applications::PropertyAnnotationOwner,
    ) {
        let bound = self.bound_annotation_spans(&property.annotations);
        let policy_depth =
            self.check_property_annotation_applications(scope, property, owner, &|annotation| {
                bound.contains(&annotation.span)
            });
        let accessors = property.getter_annotations.iter().chain(
            property
                .setter
                .iter()
                .flat_map(|setter| &setter.annotations),
        );
        for entry in accessors {
            if self.has_bound_annotation(&entry.annotation) {
                self.check_annotation_application(scope, &entry.annotation, &entry.arguments);
            }
        }
        self.active_lexical_policies.truncate(policy_depth);
    }

    pub(super) fn fold_method_annotations_in_temporary_scope(
        &mut self,
        class_scope: &CheckerScope<'_>,
        function: &FunDecl,
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
        let suppression_depth =
            self.fold_function_annotation_applications(&method_scope, function, "function");
        self.active_lexical_policies.truncate(suppression_depth);
    }

    pub(super) fn fold_annotation_values(
        &mut self,
        owner: Ty,
        arguments: &[ExprId],
        nested_call: Option<ExprId>,
    ) -> Option<FoldedAnnotationValues> {
        let internal = owner.kotlin_class_internal()?;
        let (elements, parameters, policy) = self.applied_annotation_shape(owner)?;
        let argument_names = self.annotation_argument_names(arguments, nested_call);
        let parameter_indices = self
            .annotation_argument_parameter_indices(&parameters, arguments, &argument_names)
            .ok()?;
        let mut written = Vec::with_capacity(parameter_indices.len());
        for &index in &parameter_indices {
            if !written.contains(&index) {
                written.push(index);
            }
        }
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
        let order = written
            .into_iter()
            .map(|index| {
                let position = values.iter().position(|(slot, _, _)| *slot == index)?;
                u16::try_from(position).ok()
            })
            .collect::<Option<_>>()?;
        Some(FoldedAnnotationValues {
            values: values
                .into_iter()
                .map(|(_, name, value)| (name, value))
                .collect(),
            facts,
            order,
        })
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
        let Some(selected) = self
            .resolved_enum_entries
            .get(&argument)
            .filter(|entry| entry.classifier == level)
            .and_then(|entry| usize::try_from(entry.ordinal).ok())
        else {
            return;
        };
        let is = |entry: &str| self.classifier_enum_entry_ordinal(level, entry) == Some(selected);
        facts.deprecated_hidden = is("HIDDEN");
        facts.deprecated_error = is("ERROR");
    }

    /// The targets WRONG_ANNOTATION_TARGET names for `internal`: its declared targets, or for a
    /// source class whose own `@Target` application is invalid, kotlinc's recovery from it.
    pub(super) fn diagnostic_annotation_targets(
        &self,
        internal: TypeName,
    ) -> Option<crate::types::AnnotationTargets> {
        if self.module.annotation_retention(internal).is_some() {
            if let Some(targets) = self.module.annotation_targets().diagnostic(internal) {
                return Some(targets);
            }
        }
        self.declared_annotation_targets(internal)
    }

    /// The targets `internal` declares, whether this module or a library declares the class.
    pub(super) fn declared_annotation_targets(
        &self,
        internal: TypeName,
    ) -> Option<crate::types::AnnotationTargets> {
        if self.module.annotation_retention(internal).is_some() {
            return Some(
                self.module
                    .annotation_targets()
                    .semantic(internal)
                    .unwrap_or(crate::types::AnnotationTargets::DEFAULT),
            );
        }
        Some(
            self.resolver()
                .classifier(internal)?
                .annotation_targets
                .unwrap_or(crate::types::AnnotationTargets::DEFAULT),
        )
    }

    /// Fold one application written with `use_site` (`@get:A`, or `None` when unprefixed) and
    /// naming its class `short_name`.
    pub(super) fn fold_annotation_application(
        &mut self,
        owner: Ty,
        arguments: &[ExprId],
        use_site: Option<crate::types::PropertyAnnotationSite>,
        short_name: &str,
    ) -> Option<crate::types::AppliedAnnotation> {
        let FoldedAnnotationValues {
            values,
            mut facts,
            order,
        } = self.fold_annotation_values(owner, arguments, None)?;
        let internal = owner.kotlin_class_internal()?;
        facts.optional_expectation = self.is_optional_expectation_classifier(internal);
        let module_retention = self.module.annotation_retention(internal);
        let classifier = self.resolver().classifier(internal)?;
        let retention =
            super::annotation_applications::annotation_retention(module_retention, &classifier)?;
        let targets = self.declared_annotation_targets(internal)?;
        Some(crate::types::AppliedAnnotation {
            internal,
            values,
            facts,
            written: Box::new(crate::types::WrittenAnnotation {
                short_name: short_name.into(),
                use_site,
                order,
            }),
            retention,
            targets,
            property_sites: None,
        })
    }
}

/// One application's folded values in declaration order, with the order the source wrote them.
pub(super) struct FoldedAnnotationValues {
    pub(super) values: Vec<(String, crate::types::AnnotationValue)>,
    pub(super) facts: crate::types::AnnotationSemanticFacts,
    /// See [`crate::types::WrittenAnnotation::order`].
    pub(super) order: Box<[u16]>,
}

/// Folding one checked annotation argument to the typed value the annotation records.
impl Checker<'_> {
    fn fold_annotation_value(
        &mut self,
        e: ExprId,
        expected: Option<Ty>,
    ) -> Option<crate::types::AnnotationValue> {
        use crate::synthetics::SyntheticKind;
        use crate::types::AnnotationValue;

        // Enum-entry identity is the checked semantic result and is independent of how the source
        // spelled the reference. A qualified `Kind.VALUE`, an explicitly imported `VALUE`, and a
        // context-sensitive `VALUE` therefore fold identically instead of branching on AST shape.
        if let Some(entry) = self.resolved_enum_entries.get(&e) {
            return Some(AnnotationValue::Enum(entry.classifier, entry.name.clone()));
        }

        let value = match self.file.expr(e) {
            Expr::StringLit(value) => AnnotationValue::String(value.clone()),
            // An integer LITERAL takes the element's declared width too. Emitting `I` for a
            // `byte`/`short` element writes the wrong tag just as a mistyped constant does, and
            // read-back throws `AnnotationTypeMismatchException`.
            Expr::IntLit(value) | Expr::UIntLit(value) => match expected.map(Ty::non_null) {
                Some(Ty::Byte) => AnnotationValue::Byte(i8::try_from(*value).ok()?),
                Some(Ty::Short) => AnnotationValue::Short(i16::try_from(*value).ok()?),
                Some(Ty::Long) => AnnotationValue::Long(*value),
                _ => AnnotationValue::Int(*value as i32),
            },
            Expr::LongLit(value) | Expr::ULongLit(value) => AnnotationValue::Long(*value),
            Expr::DoubleLit(value) => AnnotationValue::Double(*value),
            Expr::FloatLit(value) => AnnotationValue::Float(*value),
            Expr::BoolLit(value) => AnnotationValue::Boolean(*value),
            Expr::CharLit(value) => AnnotationValue::Char(*value),
            Expr::Template(_) => AnnotationValue::String(self.fold_annotation_string(e)?),
            Expr::Binary { op: BinOp::Add, .. }
                if self.expr_types[e.0 as usize].non_null() == Ty::String =>
            {
                AnnotationValue::String(self.fold_annotation_string(e)?)
            }
            Expr::AnnotationArrayLiteral(elements) => {
                // The DECLARED element type decides what these expressions fold to, which is why
                // the parser retained the literal instead of picking an array factory.
                let element = expected.map(Ty::non_null).and_then(Ty::array_elem);
                let elements = elements.clone();
                AnnotationValue::Array(
                    elements
                        .iter()
                        .map(|&element_expr| self.fold_annotation_value(element_expr, element))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            Expr::UnsupportedAnnotationArgument(_) => return None,
            // A cast to the type its constant operand already has (`NAME as String`) keeps the
            // operand's value; any other cast is not one kotlinc evaluates.
            Expr::As {
                operand,
                nullable: false,
                ..
            } if self.expr_types[operand.0 as usize].non_null()
                == self.expr_types[e.0 as usize].non_null() =>
            {
                return self.fold_annotation_value(*operand, expected);
            }
            // A qualified named constant reached through its owner (`Rules.AUTHENTICATED`, a
            // `const val` on a companion). Enum entries returned above from their resolved identity.
            Expr::Member { .. } => self.folded_library_constant(e, expected)?,
            Expr::CallableRef {
                receiver: Some(_),
                name,
            } if name == "class" => {
                AnnotationValue::Class(Ty::obj_name(self.class_literal_targets.get(&e).copied()?))
            }
            Expr::Call { callee, args } => match self.expr_lowers.get(&e) {
                Some(ExprLowering::CompilerSynthetic(
                    SyntheticKind::PrimitiveVararg(_)
                    | SyntheticKind::ReferenceVararg
                    | SyntheticKind::EmptyReference,
                )) => {
                    let element = expected.and_then(Ty::array_elem);
                    AnnotationValue::Array(
                        args.iter()
                            .map(|argument| self.fold_annotation_value(*argument, element))
                            .collect::<Option<Vec<_>>>()?,
                    )
                }
                _ => {
                    // A call selected as part of Kotlin's compile-time constant language (for
                    // example, `'8'.toByte()`) is an annotation value in its own right. Only a
                    // call that does not fold through its selected callable can be a nested
                    // annotation construction. Trying the checked constant first keeps callable
                    // identity authoritative and avoids reinterpreting an intrinsic by syntax.
                    if let Some(value) = self.folded_library_constant(e, expected) {
                        return Some(value);
                    }
                    let owner = self.expr_types[e.0 as usize];
                    let internal = owner.kotlin_class_internal()?;
                    let short_name = match self.file.expr(*callee) {
                        Expr::Name(name) | Expr::Member { name, .. } => name.as_str().into(),
                        _ => return None,
                    };
                    let FoldedAnnotationValues { values, order, .. } =
                        self.fold_annotation_values(owner, args, Some(e))?;
                    AnnotationValue::Annotation {
                        internal,
                        values,
                        written: Some(Box::new(crate::types::WrittenAnnotation {
                            short_name,
                            use_site: None,
                            order,
                        })),
                    }
                }
            },
            _ => self.folded_library_constant(e, expected)?,
        };
        Some(value)
    }

    /// Fold the compile-time string language after ordinary expression checking has selected every
    /// constant reference. Unlike the parser's directive-oriented string helper, this consumes the
    /// resolver's `LibraryConst` payloads, so nested-class and dependency constants participate
    /// without reopening a source spelling or classifier lookup.
    fn fold_annotation_string(&self, expression: ExprId) -> Option<crate::kt_string::KtString> {
        match self.checked_constant(expression, Ty::String)?.value {
            crate::libraries::LibConst::Str(value) => Some(value),
            _ => None,
        }
    }

    fn checked_constant(
        &self,
        expression: ExprId,
        declared: Ty,
    ) -> Option<crate::libraries::LibraryConst> {
        let context = CheckedConstantExpression {
            file: self.file,
            expression_types: &self.expr_types,
            resolved_constants: &self.resolved_constants,
            resolved_calls: &self.resolved_calls,
            resolved_operator_calls: &self.resolved_operator_calls,
        };
        checked_constant_expression(context, expression, declared)
    }

    /// A resolved compile-time constant read (`const val`), whatever expression shape reached it.
    ///
    /// The value is re-typed to the ELEMENT's declared type. `LibConst` models an integral constant
    /// as `Int` whatever its declared width, so folding it verbatim wrote a `boolean` element with
    /// the `I` tag — which javap shows as `flag=1` where kotlinc writes `Z`/`flag=true`, and which
    /// throws `AnnotationTypeMismatchException` when the annotation is read back. An element type
    /// the constant cannot honor folds to `None`, which reports the argument rather than emitting
    /// a mistyped one.
    fn folded_library_constant(
        &self,
        e: ExprId,
        expected: Option<Ty>,
    ) -> Option<crate::types::AnnotationValue> {
        use crate::libraries::LibConst;
        use crate::types::AnnotationValue;

        let declared = expected.map(Ty::non_null);
        // A constant reference was resolved by checking; any other constant expression (`-1`,
        // `LIMIT * 2`) folds through the same checked constant language as a `const val`.
        let constant = match self.resolved_constants.get(&e) {
            Some(constant) => constant.clone(),
            None => self.checked_constant(e, declared?)?,
        };
        Some(match &constant.value {
            LibConst::Int(value) => match declared {
                Some(Ty::Boolean) => AnnotationValue::Boolean(*value != 0),
                Some(Ty::Char) => AnnotationValue::Char(u16::try_from(*value).ok()?),
                Some(Ty::Long) => AnnotationValue::Long(i64::from(*value)),
                Some(Ty::Byte) => AnnotationValue::Byte(i8::try_from(*value).ok()?),
                Some(Ty::Short) => AnnotationValue::Short(i16::try_from(*value).ok()?),
                _ => AnnotationValue::Int(*value),
            },
            LibConst::Long(value) => AnnotationValue::Long(*value),
            LibConst::Float(value) => AnnotationValue::Float(*value),
            LibConst::Double(value) => AnnotationValue::Double(*value),
            LibConst::Str(value) => AnnotationValue::String(value.clone()),
        })
    }
}
