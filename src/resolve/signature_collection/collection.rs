//! The declaration walk that builds the semantic `SymbolTable`.
//!
//! The phases that can be decided before the walk now live beside it: the name universe in
//! [`super::type_universe`], the declaration-derived classifier facts in
//! [`super::declared_classifier_inventory`], and the whole-module decisions that can only be made
//! after it in [`super::supertype_cycles`] and [`super::top_level_overload_conflicts`].
//!
//! MIGRATION DEBT: what is left is still one function, well above the repository's size rule, and
//! most of it is a single loop that builds every declaration shape inline against the walk's shared
//! locals. It comes apart by shape — top-level callables, top-level and extension properties, class
//! members, constructors — once each shape's signature construction takes an explicit context
//! instead of reaching for those locals. Do not add another declaration shape to it in place.

use super::*;

pub(in crate::resolve) fn collect_signatures_with_cp_impl(
    files: &[File],
    libraries: Box<dyn SemanticPlatform>,
    frontend_plugins: &crate::plugins::PluginHost,
    diags: &mut DiagSink,
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
    compact_local_contexts: Option<&[PassOneLocalClassContext]>,
) -> SymbolTable {
    let SourceTypeUniverse {
        file_type_aliases,
        source_packages,
        source_imports,
        class_names,
        file_class_names,
        user_defined,
        user_base_classes,
    } = source_type_universe(files, &*libraries, diags, compact_headers);
    let DeclaredClassifierInventory {
        legacy_anonymous_lexical_scopes,
        legacy_local_class_tparams,
        legacy_local_class_siblings,
        source_classifier_visibility,
        source_direct_supertypes,
    } = declared_classifier_inventory(
        files,
        compact_headers,
        compact_local_contexts,
        &file_class_names,
        &user_defined,
    );

    let ty_of_ref = |r: &TypeRef, classes: &ClassNames, tparams: &TParams, diags: &mut DiagSink| {
        ty_of_ref_with(r, classes, tparams, diags)
    };
    // Helper tables are rebuilt by the authoritative pass, which owns their diagnostics.
    let ty_of_ref_silent = |r: &TypeRef, classes: &ClassNames, tparams: &TParams| {
        ty_of_ref_with(r, classes, tparams, &mut DiagSink::new())
    };

    // Pass 2: resolve signatures/properties against the now-complete type universe.
    let mut table = SymbolTable::default();
    let mut declaration_annotation_resolution_attempts = std::collections::HashSet::new();
    for file_index in 0..source_packages.len() {
        diags.set_file(file_index as u32);
        let names = &file_class_names[file_index];
        let mut annotation_resolution = DeclarationAnnotationResolution {
            file_index: file_index as u32,
            attempted: &mut declaration_annotation_resolution_attempts,
        };
        // A declaration type-parameter annotation must be bound in its declaration's lexical
        // classifier scope below. `detached_type_refs` also carries the same occurrence so Pass 1
        // can discover its spelling, but resolving that duplicate from file scope first gives a
        // nested `Host.Marker` occurrence the unrelated top-level `Marker` identity.
        let declaration_annotation_spans = files
            .get(file_index)
            .into_iter()
            .flat_map(|file| file.declaration_type_parameter_annotations.values())
            .flatten()
            .flat_map(|parameter| {
                parameter
                    .annotations
                    .iter()
                    .map(|annotation| annotation.span)
            })
            .collect::<std::collections::HashSet<_>>();
        if let Some(headers) = compact_headers {
            let source = crate::fir::SourceFileId::from_raw(file_index as u32);
            for root in headers.detached_type_roots(source) {
                let Some(reference) = headers
                    .syntax
                    .transient_type_ref(root, &headers.lookup_names)
                else {
                    continue;
                };
                if declaration_annotation_spans.contains(&reference.span) {
                    continue;
                }
                if let Some(internal) = names.get_class(&reference.name) {
                    table.resolved_annotations.insert(
                        (file_index as u32, reference.span.lo, reference.span.hi),
                        internal,
                    );
                }
            }
        } else {
            let file = &files[file_index];
            for reference in &file.detached_type_refs {
                if declaration_annotation_spans.contains(&reference.span) {
                    continue;
                }
                if let Some(internal) = names.get_class(&reference.name) {
                    table.resolved_annotations.insert(
                        (file_index as u32, reference.span.lo, reference.span.hi),
                        internal,
                    );
                }
            }
        }
        if compact_headers.is_none() {
            let file = &files[file_index];
            for &declaration_start in &file.type_alias_declaration_starts {
                resolve_declaration_type_parameter_annotations(
                    file,
                    declaration_start,
                    names,
                    &mut table.resolved_annotations,
                    &mut annotation_resolution,
                    diags,
                );
            }
            if let Some(script_body) = file.script_body {
                resolve_local_type_parameter_annotation_inventory(
                    file,
                    [script_body],
                    names,
                    &mut table.resolved_annotations,
                    &mut annotation_resolution,
                    diags,
                );
            }
        }
    }
    // A target-less optional expectation belongs to the common source set only. Its provider record
    // participates in ordinary qualified-name binding, then this source-role gate removes it from a
    // platform file before annotation checking. A real target `actual` shadows the common record and
    // therefore is not classified as optional here.
    table
        .resolved_annotations
        .retain(|(file, _, _), annotation| {
            let common = compact_headers.map_or_else(
                || {
                    files
                        .get(*file as usize)
                        .is_some_and(|source| source.is_common)
                },
                |headers| {
                    headers
                        .scopes
                        .file(crate::fir::SourceFileId::from_raw(*file))
                        .is_some_and(|source| source.is_common)
                },
            );
            let optional = libraries.is_optional_expectation(*annotation);
            crate::trace_compiler!(
            "diagnostic",
            "annotation source={file} identity={annotation:?} common={common} optional={optional}",
        );
            common || !optional
        });
    // Normalize source annotation retention once identities are bound. The explicit retention
    // argument is interpreted only under the resolved `kotlin.annotation.Retention` declaration;
    // later phases receive the enum fact and never inspect its source spelling.
    if let Some(headers) = compact_headers {
        collect_compact_annotation_policies(headers, &mut table);
    } else {
        for (file_index, file) in files.iter().enumerate() {
            for &declaration in &file.decls {
                let Decl::Class(class) = file.decl(declaration) else {
                    continue;
                };
                if class.kind != crate::ast::ClassKind::Annotation {
                    continue;
                }
                let retention = class
                    .annotations
                    .iter()
                    .zip(&class.annotation_args)
                    .find_map(|(annotation, arguments)| {
                        table
                            .resolved_annotation(file_index as u32, annotation)
                            .filter(|name| name.matches("kotlin/annotation/Retention"))?;
                        let &argument = arguments.first()?;
                        let Expr::Member { name, .. } = file.expr(argument) else {
                            return None;
                        };
                        match name.as_str() {
                            "RUNTIME" => Some(crate::types::AnnotationRetention::Runtime),
                            "BINARY" => Some(crate::types::AnnotationRetention::Binary),
                            "SOURCE" => Some(crate::types::AnnotationRetention::Source),
                            _ => None,
                        }
                    })
                    .unwrap_or(crate::types::AnnotationRetention::Default);
                let Some(annotation_identity) = file_class_names[file_index].get_class(&class.name)
                else {
                    continue;
                };
                table
                    .annotation_retentions
                    .insert(annotation_identity, retention);
                // `@Target(AnnotationTarget.X, …)` decides where an application written with no use-site
                // prefix lands. Only the three declaration sites a PROPERTY application can take are
                // modeled; an annotation class that declares no `@Target` is applicable everywhere and is
                // left out of the map entirely.
                let declared_targets = class
                    .annotations
                    .iter()
                    .zip(&class.annotation_args)
                    .find_map(|(annotation, arguments)| {
                        table
                            .resolved_annotation(file_index as u32, annotation)
                            .filter(|name| name.matches("kotlin/annotation/Target"))?;
                        let mut targets = crate::types::AnnotationTargets {
                            value_parameter: false,
                            property: false,
                            field: false,
                        };
                        for &argument in arguments {
                            // `@Target` takes a `vararg` of enum entries. The NAMED form spells the
                            // same list as an array literal or `arrayOf`, so both flatten here.
                            let entries: Vec<ExprId> = match file.expr(argument) {
                                Expr::AnnotationArrayLiteral(elements) => elements.clone(),
                                Expr::Call { args, .. } => args.to_vec(),
                                _ => vec![argument],
                            };
                            for entry in entries {
                                let Expr::Member { name, .. } = file.expr(entry) else {
                                    continue;
                                };
                                match name.as_str() {
                                    "VALUE_PARAMETER" => targets.value_parameter = true,
                                    "PROPERTY" => targets.property = true,
                                    "FIELD" => targets.field = true,
                                    _ => {}
                                }
                            }
                        }
                        Some(targets)
                    });
                if let Some(targets) = declared_targets {
                    table
                        .annotation_targets
                        .insert(annotation_identity, targets);
                }
            }
        }
    }
    // Providers normalize compiled declarations into `LibraryType`; retain only the semantic policy
    // for annotation identities actually referenced by this source set.
    normalize_referenced_library_annotations(&mut table, &*libraries);
    // Seed the complete source package namespace before declaration collection starts. Package
    // qualification is syntax-derived and does not change as provisional class/callable signatures
    // are installed below, so ModuleSymbols can query this set directly during mutation.
    if let Some(headers) = compact_headers {
        for source in 0..headers.sources.len() {
            let Some(source) = headers
                .sources
                .get(crate::fir::SourceFileId::from_raw(source as u32))
            else {
                continue;
            };
            let mut package = Some(source.package);
            while let Some(current) = package {
                if current == TypeName::ROOT || !table.source_packages.insert(current) {
                    break;
                }
                package = current.parent();
            }
        }
    } else {
        for file in files {
            let Some(package) = file.package.as_deref() else {
                continue;
            };
            let mut current = String::new();
            for segment in package.split('.').filter(|segment| !segment.is_empty()) {
                if !current.is_empty() {
                    current.push('/');
                }
                current.push_str(segment);
                table.source_packages.insert(type_name(&current));
            }
        }
    }
    // Pre-seed identity-keyed declaration headers from ALL files so lightweight initializer
    // inference is independent of declaration order. These are the exact flags later moved onto
    // each canonical `ClassSig`, not a parallel simple-name object index.
    if let Some(headers) = compact_headers {
        publish_compact_parser_classifier_identities(
            compact_local_contexts,
            files.len(),
            &mut table,
        );
        for stub in headers
            .stubs
            .iter()
            .filter(|stub| stub.kind == crate::fir::DeclarationKind::Classifier)
        {
            let (_, internal) = compact_classifier_identity(headers, stub)
                .expect("a compact classifier must retain its stable identity");
            table.source_class_headers.insert(
                internal,
                SourceClassHeader {
                    flags: streamed_source_class_flags(headers, stub),
                    direct_supertypes: source_direct_supertypes
                        .get(&internal)
                        .cloned()
                        .unwrap_or_default()
                        .into(),
                },
            );
        }
    } else {
        for file in files {
            for &declaration in &file.decls {
                if let Decl::Class(class) = file.decl(declaration) {
                    let internal = type_name(&class_internal(file, &class.name));
                    table.source_class_headers.insert(
                        internal,
                        SourceClassHeader {
                            flags: source_class_flags(class),
                            direct_supertypes: source_direct_supertypes
                                .get(&internal)
                                .cloned()
                                .unwrap_or_default()
                                .into(),
                        },
                    );
                }
            }
        }
    }
    // Same-name top-level functions are kept as overloads; a real "conflicting declarations" clash
    // is a same-*package* same-erasure duplicate. Keyed by (package, name, erased params) so a
    // cross-package homonym (a star-imported function shadowed by a local one) is not a conflict.
    let mut top_level_fun_groups = TopLevelFunctionConflictGroups::default();
    let mut pending_conflict_diagnostics = HashMap::new();
    let mut reserved_conflict_diagnostic_bytes = 0usize;
    let mut retained_conflict_display_bytes = 0usize;
    // Unannotated MEMBER expression getters whose first-pass inference hit `Error` — a referenced
    // class may simply be collected later (another file, or a later class in this one). Each entry
    // keeps the exact scope of its first pass; retried to a fixpoint after the walk (see
    // [`finish_member_computed_getter_inference`]).
    // Properties — top-level and class members alike — whose type this walk could not determine,
    // resolved afterwards by the engine (see [`resolve_deferred_properties`]). One queue, because
    // a member may be waiting on a module property and a module property on a member.
    let mut deferred_properties: Vec<DeferredProperty> = Vec::new();
    let empty_local_context = PassOneLocalClassContext::default();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        let class_names = file_class_names[i].clone();
        let legacy_anonymous_lexical_scope = legacy_anonymous_lexical_scopes
            .as_ref()
            .and_then(|scopes| scopes.get(i));
        let mut annotation_resolution = DeclarationAnnotationResolution {
            file_index: i as u32,
            attempted: &mut declaration_annotation_resolution_attempts,
        };
        let projected_parser_classifier = |parser| {
            projected_parser_classifier_identity(
                file,
                compact_headers,
                compact_local_contexts,
                i,
                parser,
            )
        };
        for &d in &file.decls {
            let compact_declaration = compact_headers.and_then(|headers| {
                compact_parser_declaration_stub(headers, compact_local_contexts, i, d)
            });
            if compact_headers.is_some() && compact_declaration.is_none() {
                continue;
            }
            match file.decl(d) {
                Decl::Fun(f) => {
                    let compact_function = compact_declaration;
                    let callable_header = match compact_headers.zip(compact_function) {
                        Some((headers, stub)) => streamed_callable_header_by_declaration(
                            headers, stub.id,
                        )
                        .expect("a production top-level function must have a compact header"),
                        _ => legacy_callable_header(f),
                    };
                    let compact_flags = compact_function.map(|stub| stub.flags);
                    let function_name = compact_function
                        .and_then(|stub| stub.lookup_name)
                        .and_then(|name| {
                            compact_headers.and_then(|headers| headers.lookup_names.get(name))
                        })
                        .unwrap_or(&f.name)
                        .to_string();
                    let function_visibility =
                        compact_function.map_or(f.visibility, |stub| stub.visibility);
                    let companion_extension = compact_flags.map_or_else(
                        || f.is_companion_extension(),
                        |flags| flags.has(crate::fir::DeclarationFlags::COMPANION),
                    );
                    let reified = if compact_headers.is_some() {
                        callable_header
                            .type_parameter_flags
                            .iter()
                            .any(|flags| flags.is_reified())
                    } else {
                        !f.reified_type_params.is_empty()
                    };
                    let (is_inline, is_operator, is_infix, is_override, is_final, is_suspend) =
                        if let Some(flags) = compact_flags {
                            (
                                flags.has(crate::fir::DeclarationFlags::INLINE),
                                flags.has(crate::fir::DeclarationFlags::OPERATOR),
                                flags.has(crate::fir::DeclarationFlags::INFIX),
                                flags.has(crate::fir::DeclarationFlags::OVERRIDE),
                                flags.has(crate::fir::DeclarationFlags::FINAL),
                                flags.has(crate::fir::DeclarationFlags::SUSPEND),
                            )
                        } else {
                            (
                                f.is_inline(),
                                f.is_operator(),
                                f.is_infix(),
                                f.is_override(),
                                f.is_final(),
                                f.is_suspend(),
                            )
                        };
                    if compact_headers.is_none() {
                        resolve_declaration_type_parameter_annotation_inventory(
                            file,
                            d,
                            &class_names,
                            &mut table.resolved_annotations,
                            &mut annotation_resolution,
                            diags,
                            true,
                        );
                    }
                    let source_key = (i as u32, d.0);
                    let value_operand_slots = if compact_headers.is_some() {
                        generic_value_operand_slots_from_header(&callable_header, &[])
                    } else {
                        generic_value_operand_slots(f, &[])
                    };
                    if !value_operand_slots.is_empty() {
                        table
                            .source_generic_value_operand_slots
                            .insert(source_key, value_operand_slots);
                    }
                    let tp = TParams::from_decl_with_primary_class(
                        &callable_header.type_parameters,
                        &callable_header.bounds,
                        &|n| class_names.get(n),
                        &|internal| {
                            table
                                .source_class_headers
                                .get(&internal)
                                .is_some_and(|header| header.flags.has(ClassFlags::INTERFACE))
                                || libraries
                                    .classifier(internal)
                                    .is_some_and(|shape| shape.is_interface())
                        },
                    );
                    let semantic_tp = TParams::symbolic_from_decl_with(
                        &callable_header.type_parameters,
                        &callable_header.bounds,
                        &|n| class_names.get(n),
                    )
                    .alpha_renamed_declaration(
                        &callable_header.type_parameters,
                        table.compilation_id,
                        i as u32,
                        callable_header.signature_start,
                    );
                    // A `vararg` parameter's runtime type is `Array<elem>`.
                    let params: Vec<Ty> = callable_header
                        .parameters
                        .iter()
                        .map(|parameter| {
                            let ty = ty_of_ref(&parameter.ty, &class_names, &tp, diags);
                            semantic_value_parameter_ty(ty, parameter.is_vararg)
                        })
                        .collect();
                    let ret = match callable_header.result {
                        StreamedResultKind::Explicit => ty_of_ref(
                            callable_header
                                .explicit_result
                                .as_ref()
                                .expect("explicit compact result type"),
                            &class_names,
                            &tp,
                            diags,
                        ),
                        StreamedResultKind::Inferred => {
                            if compact_headers.is_some() {
                                // Production inferred declarations are owned exclusively by the
                                // compact `SigExpr` graph. Running the legacy AST literal inferer in
                                // parallel retains ordinary bodies and can publish a competing
                                // approximation before graph finalization. `Pending` remains private
                                // to this provisional symbol table and is replaced by stable-ID
                                // projection before any FIR or backend boundary.
                                Ty::Pending
                            } else if let FunBody::Expr(e) = &f.body {
                                // For expression-body functions, try to infer the return type from
                                // the body literal (handles `fun f() = "literal"` etc.).  Falls back
                                // to Unit; check_fun will do a deeper inference pass and patch the
                                // canonical signature table before lowering.
                                // For an extension function, bind `this` to the receiver type so a body
                                // using it (`fun Int.double() = this * 2`) infers correctly.
                                let this_scope: Vec<(String, Ty, bool)> = callable_header
                                    .receiver
                                    .as_ref()
                                    .map(|r| {
                                        vec![(
                                            "this".to_string(),
                                            if f.is_companion_extension() {
                                                associated_companion_receiver_ty(
                                                    file,
                                                    r,
                                                    &class_names,
                                                    &tp,
                                                    diags,
                                                )
                                            } else {
                                                ty_of_ref(r, &class_names, &tp, diags)
                                            },
                                            false,
                                        )]
                                    })
                                    .unwrap_or_default();
                                let t = infer_lit_ty_scoped(
                                    InferenceSource::file(file, i as u32),
                                    *e,
                                    &class_names,
                                    &this_scope,
                                    &*libraries,
                                    &table,
                                );
                                if t != Ty::Error {
                                    t
                                } else if let Expr::Name(n) = file.expr(*e) {
                                    // Body is a bare parameter name (`fun f(x: T) = x`): infer T.
                                    callable_header
                                        .parameters
                                        .iter()
                                        .position(|parameter| &parameter.name == n)
                                        .map(|parameter| params[parameter])
                                        .unwrap_or(Ty::Pending)
                                } else {
                                    // NOT determined. Publishing `Unit` here is a wrong type, not a
                                    // missing one: every property initialized by a call to this
                                    // function then takes `Unit` as the answer and reports an
                                    // "initializer type mismatch: expected 'Unit'" that names the
                                    // wrong declaration entirely.
                                    Ty::Pending
                                }
                            } else {
                                unreachable!(
                                    "an inferred compact result requires an expression body"
                                )
                            }
                        }
                        StreamedResultKind::ImplicitUnit => Ty::Unit,
                    };
                    let ret = if callable_header.result != StreamedResultKind::Explicit {
                        inferred_declaration_ty(ret)
                    } else {
                        ret
                    };
                    let vararg_index = callable_header
                        .parameters
                        .iter()
                        .position(|parameter| parameter.is_vararg);
                    let vararg = vararg_index.is_some();
                    // Trailing params with defaults may be omitted by callers (positional only).
                    let trailing_defaults = if vararg {
                        0
                    } else {
                        callable_header
                            .parameters
                            .iter()
                            .rev()
                            .take_while(|parameter| parameter.has_default)
                            .count()
                    };
                    let required = callable_header.parameters.len() - trailing_defaults;
                    // Defaults are declaration bodies checked with earlier parameters in lexical
                    // scope. Overloaded declarations keep distinct stable identities here; the
                    // common IR and JVM descriptor select the matching `$default` realization.
                    let lambda_param_types: Vec<Vec<Ty>> = callable_header
                        .parameters
                        .iter()
                        .map(|parameter| {
                            if !parameter.ty.fun_params.is_empty() || parameter.ty.name == "<fun>" {
                                parameter
                                    .ty
                                    .fun_params
                                    .iter()
                                    .map(|r| ty_of_ref(r, &class_names, &tp, diags))
                                    .collect()
                            } else {
                                Vec::new()
                            }
                        })
                        .collect();
                    let source_receiver = callable_header
                        .receiver
                        .as_ref()
                        .map(|receiver| {
                            if companion_extension {
                                associated_companion_receiver_ty_from_spelling(
                                    receiver,
                                    callable_header.receiver_source_spelling.as_ref(),
                                    &class_names,
                                    &semantic_tp,
                                    diags,
                                )
                            } else {
                                ty_of_ref(receiver, &class_names, &semantic_tp, diags)
                            }
                        })
                        .map(|receiver| {
                            crate::symbol_resolver::declared_function_type(&*libraries, receiver)
                                .unwrap_or(receiver)
                        });
                    let mut generic_sig =
                        (!callable_header.type_parameters.is_empty()).then(|| {
                            let mut generic_header = callable_header.clone();
                            if companion_extension {
                                // The associated classifier receiver was resolved above through its
                                // alias identity. The generic signature helper resolves ordinary
                                // applied receiver types, so omit this namespace-only spelling and
                                // install the semantic receiver immediately below.
                                generic_header.receiver = None;
                            }
                            source_generic_signature_from_header(
                                &generic_header,
                                &class_names,
                                &semantic_tp,
                                ret,
                                if compact_headers.is_some() {
                                    // The compact signature graph owns expression-result
                                    // inference, including declaration type-parameter identity.
                                    // Re-reading the parser body here both duplicates that semantic
                                    // decision and prevents Pass 1 from releasing ordinary arenas.
                                    None
                                } else {
                                    inferred_return_type_parameter_from_header(
                                        file,
                                        f,
                                        &callable_header,
                                    )
                                },
                                diags,
                            )
                        });
                    if let Some(signature) = &mut generic_sig {
                        signature.receiver = source_receiver;
                        order_primary_class_bounds(
                            signature,
                            &table.source_class_headers,
                            &*libraries,
                        );
                    }
                    let projected_return_hazard = if compact_headers.is_some() {
                        has_projected_generic_return_hazard_from_header(&callable_header)
                    } else {
                        has_projected_generic_return_hazard(file, f)
                    };
                    if projected_return_hazard {
                        table.source_projected_return_hazards.insert(source_key);
                    }
                    let stable_declaration = compact_function.map(|stub| stub.id);
                    let sig = Signature {
                        params,
                        ret,
                        generic_sig,
                        projected_return_hazard,
                        flags: SigFlags::default()
                            .with_vararg(vararg)
                            .with_is_inline(is_inline)
                            .with_is_operator(is_operator)
                            .with_is_infix(is_infix)
                            .with_is_override(is_override)
                            .with_is_final(is_final)
                            .with_is_suspend(is_suspend)
                            .with_is_companion_extension(companion_extension)
                            // Reified source bodies may be emitted to make their inline body
                            // available across a compilation boundary, but their erased JVM method
                            // is not a legal direct-call fallback. Encode that semantic capability on
                            // the signature itself so every source callable origin maps it to the
                            // shared `InlineKind::MustInline` state instead of consulting a parallel
                            // declaration set or rediscovering `reified` in individual call paths.
                            .with_requires_splice(reified)
                            .with_has_reified_type_params(reified),
                        annotations: declaration_annotation_identities(
                            compact_headers,
                            stable_declaration,
                            &table.resolved_annotations,
                            || resolved_annotation_identities(&f.annotations, &class_names),
                        ),
                        equality_bound: None,
                        vararg_index,
                        required,
                        param_defaults: callable_header
                            .parameters
                            .iter()
                            .map(|parameter| parameter.has_default)
                            .collect(),
                        exact_params: if compact_headers.is_some() {
                            callable_header
                                .parameters
                                .iter()
                                .map(|parameter| {
                                    header_type_has_annotation(
                                        &parameter.type_annotations,
                                        &class_names,
                                        type_name("kotlin/internal/Exact"),
                                    )
                                })
                                .collect()
                        } else {
                            f.params
                                .iter()
                                .map(|parameter| {
                                    has_type_annotation(
                                        file,
                                        parameter.ty.span,
                                        &class_names,
                                        type_name("kotlin/internal/Exact"),
                                    )
                                })
                                .collect()
                        },
                        no_infer_params: if compact_headers.is_some() {
                            callable_header
                                .parameters
                                .iter()
                                .map(|parameter| {
                                    header_type_has_annotation(
                                        &parameter.type_annotations,
                                        &class_names,
                                        type_name("kotlin/internal/NoInfer"),
                                    )
                                })
                                .collect()
                        } else {
                            f.params
                                .iter()
                                .map(|parameter| {
                                    has_type_annotation(
                                        file,
                                        parameter.ty.span,
                                        &class_names,
                                        type_name("kotlin/internal/NoInfer"),
                                    )
                                })
                                .collect()
                        },
                        implicit_integer_coercion: if compact_headers.is_some() {
                            callable_header
                                .parameters
                                .iter()
                                .map(|parameter| {
                                    header_type_has_annotation(
                                        &parameter.annotations,
                                        &class_names,
                                        type_name("kotlin/internal/ImplicitIntegerCoercion"),
                                    )
                                })
                                .collect()
                        } else {
                            f.params
                                .iter()
                                .map(|parameter| {
                                    has_implicit_integer_coercion_annotation(
                                        &parameter.annotations,
                                        &class_names,
                                    )
                                })
                                .collect()
                        },
                        // Checked default expressions are body units. The compact production path
                        // retains only the declaration-level `has_default` fact above; calls carry
                        // an omitted parameter ordinal into FIR and the declaration's checked
                        // default body supplies its value. Literal payload folding remains only for
                        // the legacy AST lowerer while that non-production entry point exists.
                        param_default_values: if compact_headers.is_some() {
                            vec![None; callable_header.parameters.len()]
                        } else {
                            f.params
                                .iter()
                                .map(|p| {
                                    p.default.and_then(|dx| {
                                        extract_ctor_default(file, dx, &class_names, &*libraries)
                                    })
                                })
                                .collect()
                        },
                        param_names: callable_header
                            .parameters
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect(),
                        lambda_param_types,
                        lambda_recv: callable_header
                            .parameters
                            .iter()
                            .map(|parameter| parameter.ty.fun_has_receiver())
                            .collect(),
                        visibility: function_visibility,
                        context_count: callable_header.context_count,
                        source_decl: Some(d),
                        stable_declaration,
                        source_file: Some(i as u32),
                        source_member: None,
                        source_receiver,
                        package: source_packages[i].replace('.', "/"),
                        contract: None,
                        plugin_expression: None,
                    };
                    let jvm_name = match compact_headers.zip(stable_declaration) {
                        Some((headers, declaration)) => resolved_compact_jvm_name(
                            headers,
                            declaration,
                            &callable_header.annotations,
                            &class_names,
                            &function_name,
                        ),
                        None => resolved_jvm_name(file, f, &class_names),
                    };
                    if jvm_name != function_name {
                        table
                            .toplevel_jvm_names
                            .insert((i as u32, d.0), jvm_name.clone());
                    }
                    let retained = if compact_headers.is_some() {
                        // Production conflict classification needs finalized inferred returns.
                        // The stable post-solver pass below owns that decision; provisional
                        // `Pending` signatures must not decide whether `main` is an entry point.
                        true
                    } else {
                        let current = TopLevelFunctionConflictDecl {
                            file: i as u32,
                            declaration: TopLevelFunctionConflictDeclaration::Legacy(d),
                            diagnostic_span: f.signature_span,
                        };
                        let private = function_visibility.is_private();
                        let entry_point = is_kotlin_main_entry_point(f, &sig.params, sig.ret);
                        TopLevelFunctionConflictKey::from_signature(&sig, function_name.clone())
                            .is_none_or(|key| {
                                register_top_level_function_conflict(
                                    TopLevelFunctionConflictDisplaySource::Legacy(files),
                                    &mut top_level_fun_groups,
                                    TopLevelFunctionConflictRegistration {
                                        key,
                                        declaration: current,
                                        private,
                                        entry_point,
                                    },
                                    &mut pending_conflict_diagnostics,
                                    &mut reserved_conflict_diagnostic_bytes,
                                    &mut retained_conflict_display_bytes,
                                )
                            })
                    };
                    if callable_header.receiver.is_some() {
                        let recv_ty = sig
                            .source_receiver
                            .expect("extension signature has a resolved source receiver");
                        let source_receiver = recv_ty.extension_recv_key();
                        if retained {
                            let overloads = table
                                .ext_funs
                                .entry(function_name.clone())
                                .or_default()
                                .entry(source_receiver)
                                .or_default();
                            let overload_index = overloads.len();
                            overloads.push(sig);
                            table.source_ext_funs.insert(
                                source_key,
                                (function_name.clone(), source_receiver, overload_index),
                            );
                        }
                    } else {
                        // A platform declaration clash is decided on the emitted JVM NAME, not the
                        // source name: `g(String)` and `g(String?)` erase to the same descriptor and
                        // clash only while both are spelled `g`, so an `@JvmName` on either one
                        // separates them (kotlinc's rule). Overload SELECTION is unaffected — the
                        // source name still keys `table.funs` above.
                        if retained {
                            table.funs.entry(function_name).or_default().push(sig);
                        }
                    }
                }
                Decl::Class(c) => {
                    let compact_classifier = compact_declaration;
                    let classifier_header = match compact_headers {
                        Some(headers) => streamed_classifier_header_by_declaration(
                            headers,
                            compact_classifier
                                .expect("a production classifier must have a compact identity")
                                .id,
                        )
                        .expect("a production classifier must have a compact header"),
                        _ => legacy_classifier_header(c),
                    };
                    let classifier_flags = compact_headers.zip(compact_classifier).map_or_else(
                        || source_class_flags(c),
                        |(headers, stub)| streamed_source_class_flags(headers, stub),
                    );
                    let classifier_visibility = compact_classifier
                        .map(|stub| stub.visibility)
                        .unwrap_or(c.visibility);
                    let classifier_callable_order =
                        compact_headers.zip(compact_classifier).map_or_else(
                            || source_declared_callable_order(c),
                            |(headers, stub)| streamed_declared_callable_order(headers, stub.id),
                        );
                    let classifier_declaration_flags = compact_classifier.map(|stub| stub.flags);
                    let classifier_is_data = classifier_declaration_flags
                        .map_or(c.is_data, |flags| {
                            flags.has(crate::fir::DeclarationFlags::DATA)
                        });
                    let classifier_is_enum = classifier_declaration_flags
                        .map_or(c.is_enum(), |flags| {
                            flags.has(crate::fir::DeclarationFlags::ENUM)
                        });
                    let classifier_is_value = classifier_declaration_flags
                        .map_or(c.is_value, |flags| {
                            flags.has(crate::fir::DeclarationFlags::VALUE)
                        });
                    if compact_headers.is_none() {
                        assert_eq!(
                            classifier_header.primary_parameters.len(),
                            c.props.len(),
                            "legacy classifier and constructor parameters must stay aligned",
                        );
                    }
                    let anonymous_object = compact_classifier.map_or_else(
                        || {
                            legacy_anonymous_lexical_scope
                                .is_some_and(|scope| scope.declarations.contains(&d))
                        },
                        |stub| {
                            stub.flags
                                .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
                        },
                    );
                    // A compact classifier's stable identity is authoritative. Its source path is
                    // lookup input and need not be unique across files.
                    let internal = projected_classifier_identity(
                        file,
                        compact_headers,
                        compact_classifier,
                        &c.name,
                        &user_defined,
                        &class_names,
                    );
                    if anonymous_object {
                        table.anonymous_object_types.insert((i as u32, d), internal);
                    }
                    // A parser-hoisted body-local classifier has its explicit header types
                    // captured on the compact signature graph while the exact lexical type-alias
                    // rung is live. The table built here is only the declaration-shape bootstrap
                    // needed to evaluate that graph; it must not diagnose its necessarily
                    // incomplete spelling view. Graph evaluation below resolves these types,
                    // rejects failures, and publishes the resulting semantic header before Pass 2.
                    let compact_local_header = compact_classifier.is_some_and(|stub| {
                        stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                            || stub
                                .flags
                                .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
                    });
                    // An `inner class` captures the enclosing instance, so the outer class's type
                    // parameters are in scope for its own member/ctor/field types (`inner class N :
                    // Iterator<T>` where `T` is the outer's parameter). Walk the `inner_of` chain and
                    // include each enclosing class's parameters (erased, like the class's own).
                    // A LOCAL class is written inside another declaration's body, so that
                    // declaration's type parameters are in scope for its member types too — the
                    // hoisted `Decl::Class` collected here has lost that context.
                    let mut enclosing_tparam_declarations = compact_classifier
                        .and_then(|stub| {
                            compact_local_contexts
                                .and_then(|contexts| contexts.get(i))
                                .and_then(|context| context.enclosing_type_parameters.get(&stub.id))
                                .cloned()
                        })
                        .or_else(|| {
                            legacy_local_class_tparams
                                .as_ref()
                                .and_then(|contexts| contexts.get(i))
                                .and_then(|context| context.get(&d))
                                .cloned()
                        })
                        .unwrap_or_default();
                    if compact_headers.is_none() {
                        let mut outer = c.inner_of.clone();
                        let mut guard = 0;
                        let mut outer_declarations = Vec::new();
                        while let Some(on) = outer {
                            guard += 1;
                            if guard > 32 {
                                break;
                            }
                            if let Some(oc) = file
                                .decls
                                .iter()
                                .filter_map(|&d| match file.decl(d) {
                                    Decl::Class(x) => Some(x),
                                    _ => None,
                                })
                                .find(|x| x.name == on)
                            {
                                outer_declarations.push(EnclosingTypeParameterDeclaration {
                                    declaration_start: oc.span.lo,
                                    names: oc.type_params.clone(),
                                    bounds: oc.type_param_bounds.clone(),
                                });
                                outer = oc.inner_of.clone();
                            } else {
                                break;
                            }
                        }
                        outer_declarations.reverse();
                        enclosing_tparam_declarations.splice(0..0, outer_declarations);
                    }
                    // A classifier header is resolved before declarations in that classifier's own
                    // body enter scope. Keep this declaration-owned header scope separate from the
                    // member scope built below: `class C : Base { interface Base }` must bind the
                    // supertype to the outer/imported `Base`, while member signatures may bind the
                    // nested `C.Base`. Local body siblings and inherited nested classifiers are
                    // header-visible; declarations owned by this class are added only to the member
                    // scope below.
                    let mut header_class_names = class_names.clone();
                    // The classifier being declared is visible recursively in its own header. A
                    // nested declaration is indexed globally by its qualified source name, so its
                    // simple spelling is otherwise absent here (`Container.SomeClass :
                    // SomeInterface<SomeClass>`). This adds only the declaration itself; own nested
                    // body declarations remain excluded as required by the header-scope rule above.
                    if let Some(simple) = c.name.rsplit('.').next() {
                        header_class_names.insert_name(simple.to_owned(), internal);
                    }
                    let stable_siblings = projected_sibling_classifiers(
                        compact_headers,
                        compact_local_contexts,
                        i,
                        compact_classifier,
                    );
                    let legacy_siblings = legacy_local_class_siblings
                        .as_ref()
                        .and_then(|contexts| contexts.get(i))
                        .and_then(|context| context.get(&d))
                        .cloned();
                    for (simple, internal) in
                        stable_siblings.or(legacy_siblings).into_iter().flatten()
                    {
                        header_class_names.insert_name(simple, internal);
                    }
                    let mut lexical_inheritors = match (compact_headers, compact_classifier) {
                        (Some(headers), Some(stub)) => compact_declaration_lexical_class_names(
                            headers,
                            compact_local_contexts
                                .and_then(|contexts| contexts.get(i))
                                .unwrap_or(&empty_local_context),
                            stub.id,
                            |candidate| source_direct_supertypes.contains_key(&candidate),
                        ),
                        _ => declaration_lexical_class_names(
                            file,
                            d,
                            legacy_anonymous_lexical_scope
                                .expect("legacy anonymous classifier scope"),
                            |candidate| source_direct_supertypes.contains_key(&candidate),
                        ),
                    };
                    if compact_headers.is_none() {
                        if let Some(owner) = file.local_class_lexical_classifier_owners.get(&d) {
                            let owner = type_name(&class_internal(file, owner));
                            if !lexical_inheritors.contains(&owner) {
                                // An enum entry has no `Decl`, so splice its parser-recorded scope
                                // immediately before its real containing enum. Keeping all actual
                                // local/nested owners nearer preserves ordinary lexical shadowing:
                                // `[Nested, Local, E.A, E]`, never `[Nested, E.A, Local, E]`.
                                let rank = owner
                                    .nested_owner()
                                    .and_then(|containing| {
                                        lexical_inheritors
                                            .iter()
                                            .position(|&candidate| candidate == containing)
                                    })
                                    .unwrap_or(lexical_inheritors.len());
                                lexical_inheritors.insert(rank, owner);
                            }
                        }
                    }
                    // A classifier header cannot see declarations nested in the classifier being
                    // declared, but it does see nested classifiers of every enclosing lexical
                    // owner. Keep that distinction in the bootstrap view too: the compact graph
                    // later resolves the same source header authoritatively, and an earlier
                    // file-scope-only attempt must neither publish `Error` arguments nor leave a
                    // stale unresolved diagnostic (`object : Base<Inner>()` inside `Outer`, where
                    // `Inner` is an `Outer` nested classifier).
                    let header_lexical_owner_ranks = lexical_inheritors
                        .iter()
                        .copied()
                        .filter(|owner| *owner != internal)
                        .enumerate()
                        .map(|(rank, owner)| (owner, rank))
                        .collect::<HashMap<_, _>>();
                    extend_lexical_nested_classifier_names(
                        file,
                        &header_lexical_owner_ranks,
                        &mut header_class_names,
                    );
                    let mut referenced_types = std::collections::HashSet::new();
                    if let Some(headers) = compact_headers {
                        let source = crate::fir::SourceFileId::from_raw(i as u32);
                        for stub in headers.stubs.iter().filter(|stub| {
                            stub.source == source
                                && compact_classifier.is_some_and(|classifier| {
                                    streamed_declaration_is_within(headers, stub.id, classifier.id)
                                })
                        }) {
                            for root in headers.syntax.declaration_type_roots(stub.id) {
                                if let Some(reference) = headers
                                    .syntax
                                    .transient_type_ref(root, &headers.lookup_names)
                                {
                                    collect_typeref_names(&reference, &mut referenced_types);
                                }
                            }
                        }
                    } else {
                        collect_class_type_names(file, c, &mut referenced_types);
                    }
                    let extend_inherited_names =
                        |class_names: &mut ClassNames,
                         lexical_inheritors: &[TypeName],
                         only_if_unbound: bool| {
                            for name in &referenced_types {
                                if only_if_unbound && class_names.contains_binding(name) {
                                    continue;
                                }
                                let mut inherited =
                                    crate::symbol_resolver::InheritedNestedClassifier::NotFound;
                                for &lexical_inheritor in lexical_inheritors {
                                    let roots = source_direct_supertypes
                                        .get(&lexical_inheritor)
                                        .cloned()
                                        .unwrap_or_default();
                                    inherited =
                                        crate::symbol_resolver::inherited_nested_classifier_name(
                                            name,
                                            roots,
                                            |owner| {
                                                source_direct_supertypes
                                                    .get(&owner)
                                                    .cloned()
                                                    .unwrap_or_else(|| {
                                                        crate::symbol_resolver::direct_supertypes(
                                                            &*libraries,
                                                            Ty::obj_name(owner),
                                                        )
                                                        .into_iter()
                                                        .filter_map(Ty::obj_internal)
                                                        .collect()
                                                    })
                                            },
                                            |candidate| {
                                                source_classifier_visibility
                                                    .get(&candidate)
                                                    .copied()
                                                    .is_some_and(|visibility| {
                                                        visibility != Visibility::Private
                                                    })
                                                    || crate::symbol_resolver::inherited_classifier_shape(
                                                        &*libraries,
                                                        candidate,
                                                        lexical_inheritor,
                                                    )
                                                    .is_some()
                                            },
                                        );
                                    if inherited
                                        != crate::symbol_resolver::InheritedNestedClassifier::NotFound
                                    {
                                        break;
                                    }
                                }
                                match inherited {
                                    crate::symbol_resolver::InheritedNestedClassifier::Found(
                                        inherited,
                                    ) => {
                                        class_names.insert_name(name.clone(), inherited);
                                    }
                                    crate::symbol_resolver::InheritedNestedClassifier::Ambiguous => {
                                        class_names.mark_ambiguous(name.clone());
                                    }
                                    crate::symbol_resolver::InheritedNestedClassifier::NotFound => {}
                                }
                            }
                        };
                    // The class being declared is not an implicit-receiver rung while its own
                    // header is resolved. Enclosing lexical receiver rungs still contribute their
                    // inherited classifiers before file scope; the current class contributes its
                    // inherited classifiers only as the later fallback installed below.
                    let header_inheritors = lexical_inheritors
                        .iter()
                        .copied()
                        .filter(|owner| *owner != internal)
                        .collect::<Vec<_>>();
                    extend_inherited_names(&mut header_class_names, &header_inheritors, false);
                    // The classifier's own inherited nested classifiers form a later header rung:
                    // package/import/lexical declarations win, but an otherwise unresolved bare
                    // type may still come from a supertype (`class Child(x: Category) : Parent()`).
                    // Keeping this as a fallback also means two peer supertypes cannot make an
                    // existing package classifier spuriously ambiguous.
                    extend_inherited_names(
                        &mut header_class_names,
                        std::slice::from_ref(&internal),
                        true,
                    );
                    let lexical_owner_ranks = lexical_inheritors
                        .iter()
                        .copied()
                        .enumerate()
                        .map(|(rank, owner)| (owner, rank))
                        .collect::<HashMap<_, _>>();
                    // Primary-constructor parameter declarations see nested declarations of the
                    // class itself. They are not part of the supertype header scope, but Kotlin
                    // hoists them above package/import and inherited fallbacks (`class C(x: N) {
                    // class N }`).
                    let primary_header_class_names = {
                        let mut ext = header_class_names.clone();
                        extend_lexical_nested_classifier_names(
                            file,
                            &lexical_owner_ranks,
                            &mut ext,
                        );
                        ext
                    };
                    // Bring this class's own NESTED types into scope by their SIMPLE name (`Inner` →
                    // `Outer$Inner`), so a member's parameter/return/field type may reference a sibling
                    // nested type unqualified (`fun m(i: Inner)`) — Kotlin's nested-type scoping. A nested
                    // type is a hoisted top-level `Decl::Class` named `Outer.Inner`; map its last segment.
                    let class_names = {
                        let mut ext = header_class_names.clone();
                        // Entering the body introduces this class's receiver rung. Its inherited
                        // nested classifiers now precede those of enclosing receivers and the file
                        // scope, while declarations owned directly by the class are added below and
                        // retain the nearest lexical priority.
                        extend_inherited_names(&mut ext, std::slice::from_ref(&internal), false);
                        // Own nested classifiers from every lexical owner are in scope inside a nested
                        // class. For `Outer { inner class First; inner class Second(val first: First) }`,
                        // `First` belongs to `Outer`, not `Outer.Second`, so probing only `c.name` loses
                        // the sibling during signature collection. Index direct children of all lexical
                        // owners in one declaration scan; a nearer owner wins when names shadow.
                        extend_lexical_nested_classifier_names(
                            file,
                            &lexical_owner_ranks,
                            &mut ext,
                        );
                        ext
                    };
                    resolve_class_body_annotation_references(
                        file,
                        d,
                        c,
                        &class_names,
                        i as u32,
                        &mut table.resolved_annotations,
                    );
                    // JVM erasure of every type parameter in scope: the enclosing declarations'
                    // (outer/local) first, then this class's own. A declared reference bound erases to
                    // the bound (`<T : Cargo>` → `Lapp/Cargo;`) — the class counterpart of what the
                    // function path already does — so the resolver hands lowering the same erased type
                    // for a class parameter that kotlinc signs its members with. Enclosing declarations
                    // are folded first so a same-spelled own formal shadows them, as the symbolic
                    // scope built below does.
                    let ctp = enclosing_tparam_declarations
                        .iter()
                        .fold(TParams::default(), |scope, declaration| {
                            scope.erased_extended_with(
                                &declaration.names,
                                &declaration.bounds,
                                &|name| class_names.get(name),
                            )
                        })
                        .erased_extended_with(
                            &classifier_header.type_parameters,
                            &classifier_header.bounds,
                            &|name| class_names.get(name),
                        );
                    // An enum entry contributes a nearer lexical classifier rung than its enum.
                    // Bind entry-owned declarations first so the general class inventory cannot
                    // claim the same source occurrence through the enclosing enum's view.
                    for entry in &c.enum_entries {
                        resolve_enum_entry_type_parameter_annotation_inventory(
                            file,
                            DeclaredEnumEntry { class: c, entry },
                            &class_names,
                            &mut table.resolved_annotations,
                            &mut annotation_resolution,
                            diags,
                            compact_headers.is_none(),
                        );
                    }
                    resolve_declaration_type_parameter_annotation_inventory(
                        file,
                        d,
                        &class_names,
                        &mut table.resolved_annotations,
                        &mut annotation_resolution,
                        diags,
                        compact_headers.is_none(),
                    );
                    let mut enclosing_semantic_parameters = Vec::new();
                    let symbolic_enclosing_tparams = enclosing_tparam_declarations.iter().fold(
                        TParams::default(),
                        |scope, declaration| {
                            let extended = scope
                                .symbolic_extended_with(
                                    &declaration.names,
                                    &declaration.bounds,
                                    &|name| class_names.get(name),
                                )
                                .alpha_renamed_declaration(
                                    &declaration.names,
                                    table.compilation_id,
                                    i as u32,
                                    declaration.declaration_start,
                                );
                            enclosing_semantic_parameters.push(
                                declaration
                                    .names
                                    .iter()
                                    .map(|source| {
                                        let parameter = extended.bound(source);
                                        (
                                            source.clone(),
                                            parameter.ty_param_name().unwrap_or(source).to_string(),
                                            parameter.ty_param_bound().unwrap_or_else(|| {
                                                Ty::nullable(Ty::obj("kotlin/Any"))
                                            }),
                                        )
                                    })
                                    .collect::<Vec<_>>(),
                            );
                            extended
                        },
                    );
                    let symbolic_ctp = symbolic_enclosing_tparams
                        .symbolic_extended_with(
                            &classifier_header.type_parameters,
                            &classifier_header.bounds,
                            &|name| class_names.get(name),
                        )
                        .alpha_renamed_declaration(
                            &classifier_header.type_parameters,
                            table.compilation_id,
                            i as u32,
                            c.span.lo,
                        );
                    // Capture only type parameters lexically visible at the local declaration. Keep
                    // every declaration in `symbolic_enclosing_tparams` above so an outer bound can
                    // retain identities it mentions, but a nearer same-spelled formal hides the outer
                    // one from the local classifier itself (`class Outer<T> { fun <T> f() { class L } }`).
                    let declared_source_names = classifier_header
                        .type_parameters
                        .iter()
                        .map(String::as_str)
                        .collect::<std::collections::HashSet<_>>();
                    let projected_capture_names = classifier_header
                        .lexical_type_parameter_captures
                        .iter()
                        .map(String::as_str)
                        .collect::<std::collections::HashSet<_>>();
                    let parser_hoisted = anonymous_object || file.is_local_declaration(d);
                    let mut visible_source_names = std::collections::HashSet::new();
                    let mut visible_enclosing_parameters = enclosing_semantic_parameters;
                    for declaration in visible_enclosing_parameters.iter_mut().rev() {
                        declaration.retain(|(source, _, _)| {
                            !declared_source_names.contains(source.as_str())
                                && (!parser_hoisted
                                    || projected_capture_names.contains(source.as_str()))
                                && visible_source_names.insert(source.clone())
                        });
                    }
                    // Metadata order is outermost declaration first. Applied receiver arguments use
                    // the opposite, nearest-owner-first order.
                    let metadata_captured_type_parameters = visible_enclosing_parameters
                        .iter()
                        .flatten()
                        .map(|(_, name, _)| name.clone())
                        .collect::<Vec<_>>();
                    let captured_parameters = visible_enclosing_parameters
                        .into_iter()
                        .rev()
                        .flatten()
                        .map(|(_, name, bound)| (name, bound))
                        .collect::<Vec<_>>();
                    let captured_semantic_tparam_names = captured_parameters
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>();
                    if compact_headers.is_none() {
                        // Legacy AST lowering cannot preserve this initialization-order edge. The
                        // production path streams init blocks and property initializers as ordered
                        // checked body units, so signature collection must neither inspect the
                        // blocks nor reject otherwise-valid Kotlin merely because a member call
                        // precedes a later initializer.
                        let own_methods: std::collections::HashSet<&str> =
                            c.methods.iter().map(|m| m.name.as_str()).collect();
                        let is_own_call = |ce: ExprId| matches!(file.expr(ce), Expr::Call { callee, .. } if matches!(file.expr(*callee), Expr::Name(n) if own_methods.contains(n.as_str())));
                        if let Some(last_prop) = c
                            .init_order
                            .iter()
                            .rposition(|i| matches!(i, ClassInit::PropInit(_)))
                        {
                            for (pos, init) in c.init_order.iter().enumerate() {
                                if let (true, ClassInit::Block(b)) = (pos < last_prop, init) {
                                    if let Expr::Block { stmts, trailing } = file.expr(*b) {
                                        let calls_own = trailing.is_some_and(&is_own_call)
                                            || stmts.iter().any(|&st| matches!(file.stmt(st), Stmt::Expr(ce) if is_own_call(*ce)));
                                        if calls_own {
                                            diags.error(c.span, "krusty: an init block that calls a member method before a later property initializer is not supported (init order)".to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // All primary-ctor params (in order) define the constructor signature.
                    let ctor_params: Vec<Ty> = classifier_header
                        .primary_parameters
                        .iter()
                        .map(|parameter| {
                            let ty = if compact_local_header {
                                ty_of_ref_silent(&parameter.ty, &primary_header_class_names, &ctp)
                            } else {
                                ty_of_ref(&parameter.ty, &primary_header_class_names, &ctp, diags)
                            };
                            semantic_value_parameter_ty(ty, parameter.is_vararg)
                        })
                        .collect();
                    let ctor_param_shapes: Vec<(Ty, bool)> = classifier_header
                        .primary_parameters
                        .iter()
                        .map(|parameter| {
                            let ty = ty_of_ref_silent(
                                &parameter.ty,
                                &primary_header_class_names,
                                &symbolic_ctp,
                            );
                            let ty = semantic_value_parameter_ty(ty, parameter.is_vararg);
                            (
                                ty,
                                !parameter.is_vararg && parameter.ty.definitely_non_null(),
                            )
                        })
                        .collect();
                    let ctor_param_names: Vec<(String, bool)> = classifier_header
                        .primary_parameters
                        .iter()
                        .map(|parameter| (parameter.name.clone(), parameter.has_default))
                        .collect();
                    let ctor_implicit_integer_coercion = classifier_header
                        .primary_parameters
                        .iter()
                        .map(|parameter| {
                            header_type_has_annotation(
                                &parameter.annotations,
                                &primary_header_class_names,
                                type_name("kotlin/internal/ImplicitIntegerCoercion"),
                            )
                        })
                        .collect::<Vec<_>>();
                    // Primary-constructor defaults are executable checked body units. Compact
                    // signatures retain their presence in `ctor_param_names`; they must not retain
                    // or inspect initializer syntax merely to support legacy literal substitution.
                    let ctor_defaults: Vec<Option<CtorDefaultValue>> = if compact_headers.is_some()
                    {
                        vec![None; classifier_header.primary_parameters.len()]
                    } else {
                        c.props
                            .iter()
                            .map(|p| {
                                p.default.and_then(|dx| {
                                    extract_ctor_default(file, dx, &class_names, &*libraries)
                                })
                            })
                            .collect()
                    };
                    // Only `val`/`var` params (+ body props) are backing-field properties.
                    let mut props: Vec<(String, Ty, bool)> = classifier_header
                        .primary_parameters
                        .iter()
                        .zip(&ctor_params)
                        .filter(|(p, _)| p.is_property)
                        .map(|(p, &ty)| (p.name.clone(), ty, p.is_mutable_property))
                        .collect();
                    let mut declared_props = classifier_header
                        .primary_parameters
                        .iter()
                        .zip(&ctor_params)
                        .enumerate()
                        .filter(|(_, (header, _))| header.is_property)
                        .map(|(property_index, (header, &ty))| {
                            (
                                header.name.clone(),
                                DeclaredPropertySig {
                                    ty,
                                    storage_ty: None,
                                    visibility: header.visibility,
                                    source_visible: true,
                                    is_const: false,
                                    annotations: declaration_annotation_identities(
                                        compact_headers,
                                        header.stable_declaration,
                                        &table.resolved_annotations,
                                        || {
                                            resolved_header_annotation_identities(
                                                &header.annotations,
                                                &class_names,
                                            )
                                        },
                                    ),
                                    getter_name: if classifier_flags.has(ClassFlags::ANNOTATION) {
                                        header.name.clone()
                                    } else {
                                        property_getter_name(&header.name)
                                    },
                                    setter_name: header
                                        .is_mutable_property
                                        .then(|| property_setter_name(&header.name)),
                                    setter_parameter_name: None,
                                    setter_visibility: header
                                        .is_mutable_property
                                        .then_some(header.visibility),
                                    has_custom_getter: false,
                                    is_abstract: classifier_flags.has(ClassFlags::ANNOTATION),
                                    is_open: header.is_open,
                                    context_params: Vec::new(),
                                    source_member: compact_headers.is_none().then_some(
                                        crate::libraries::SourceMember::ClassProperty {
                                            file: i as u32,
                                            owner: d.0,
                                            property: property_index as u32,
                                        },
                                    ),
                                    stable_declaration: header.stable_declaration,
                                },
                            )
                        })
                        .collect::<HashMap<_, _>>();
                    let mut contextual_props = HashMap::<String, Vec<DeclaredPropertySig>>::new();
                    let mut member_ext_props = HashMap::new();
                    let mut member_ext_keys = std::collections::HashSet::new();
                    // Body properties (`class C { val x = … }`) are also fields/accessors. A computed
                    // property (custom getter, no annotation) infers its type from the getter body.
                    // Initializer scope: ALL primary-ctor params (property or not — they're in scope for a
                    // property initializer) plus each preceding body property, so `val y = x*2` sees the
                    // ctor param `x` and `val z = y+1` sees the earlier `y`.
                    let mut init_scope: Vec<(String, Ty, bool)> = Vec::new();
                    // An inner classifier's initializer also sees properties of its captured outer
                    // receiver. For an enum-entry inner class the lexical name includes the entry
                    // (`E.ENTRY.Inner`) while `inner_of` correctly names the semantic receiver (`E`),
                    // so include that entry body's properties after the enum's ordinary properties.
                    if let Some(outer_name) = c.inner_of.as_deref() {
                        if let Some(outer) = file.decls.iter().find_map(|&declaration| {
                            matches!(file.decl(declaration), Decl::Class(owner) if owner.name == outer_name)
                                .then(|| match file.decl(declaration) {
                                    Decl::Class(owner) => owner,
                                    _ => unreachable!(),
                                })
                        }) {
                            let outer_tparams = TParams::from_decl_with(
                                &outer.type_params,
                                &outer.type_param_bounds,
                                &|name| class_names.get(name),
                            );
                            init_scope.extend(outer.props.iter().filter(|p| p.is_property).map(|p| {
                                let ty = ty_of_ref_silent(&p.ty, &class_names, &outer_tparams);
                                (
                                    p.name.clone(),
                                    semantic_value_parameter_ty(ty, p.is_vararg),
                                    p.is_var,
                                )
                            }));
                            for property in &outer.body_props {
                                let ty = property
                                    .ty
                                    .as_ref()
                                    .map(|ty| {
                                        ty_of_ref_silent(ty, &class_names, &outer_tparams)
                                    })
                                    .or_else(|| {
                                        property.init.map(|expression| {
                                            if compact_headers.is_some() {
                                                return Ty::Pending;
                                            }
                                            infer_lit_ty_scoped(
                                                InferenceSource::file(file, i as u32),
                                                expression,
                                                &class_names,
                                                &init_scope,
                                                &*libraries,
                                                &table,
                                            )
                                        })
                                    })
                                    .unwrap_or(Ty::Error);
                                init_scope.push((property.name.clone(), ty, property.is_var));
                            }
                            let entry_name = c
                                .name
                                .strip_prefix(&format!("{}.", outer.name))
                                .and_then(|tail| tail.split('.').next());
                            if let Some(entry) = entry_name.and_then(|entry_name| {
                                outer
                                    .enum_entries
                                    .iter()
                                    .find(|entry| entry.name == entry_name)
                            }) {
                                for property in entry
                                    .props
                                    .iter()
                                    .filter(|property| property.span.lo < c.span.lo)
                                {
                                    let ty = property
                                        .ty
                                        .as_ref()
                                        .map(|ty| {
                                            ty_of_ref_silent(ty, &class_names, &outer_tparams)
                                        })
                                        .or_else(|| {
                                            property.init.map(|expression| {
                                                if compact_headers.is_some() {
                                                    return Ty::Pending;
                                                }
                                                infer_lit_ty_scoped(
                                                    InferenceSource::file(file, i as u32),
                                                    expression,
                                                    &class_names,
                                                    &init_scope,
                                                    &*libraries,
                                                    &table,
                                                )
                                            })
                                        })
                                        .unwrap_or(Ty::Error);
                                    init_scope.push((property.name.clone(), ty, property.is_var));
                                }
                            }
                        }
                    }
                    init_scope.extend(
                        classifier_header
                            .primary_parameters
                            .iter()
                            .zip(&ctor_params)
                            .map(|(p, &ty)| (p.name.clone(), ty, p.is_mutable_property)),
                    );
                    let direct_companion = c.companion.and_then(projected_parser_classifier);
                    // Publish explicitly typed members before any property/getter initializer asks
                    // for them. This is not a spelling-to-return shortcut: the temporary class
                    // header is consumed through ModuleSymbols, so overload selection, generic
                    // binding, visibility, and receiver priority are identical to the checker path.
                    let header_type_parameters = crate::types::TypeParameters::new(
                        classifier_header
                            .type_parameters
                            .iter()
                            .map(|source| {
                                symbolic_ctp
                                    .bound(source)
                                    .ty_param_name()
                                    .unwrap_or(source)
                                    .to_string()
                            })
                            .collect(),
                        classifier_header
                            .type_parameters
                            .iter()
                            .map(|source| {
                                symbolic_ctp
                                    .bound(source)
                                    .ty_param_bound()
                                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                            })
                            .collect(),
                        classifier_header.type_parameter_variances.clone(),
                    );
                    let header_captured_type_parameters = crate::types::TypeParameters::invariant(
                        captured_parameters
                            .iter()
                            .map(|(name, _)| name.clone())
                            .collect(),
                        captured_parameters
                            .iter()
                            .map(|(_, bound)| *bound)
                            .collect(),
                    );
                    let (declared_methods, declared_member_extensions) =
                        declared_member_callable_headers(
                            &DeclaredMemberHeaderContext {
                                file,
                                classes: &class_names,
                                class_tparams: &ctp,
                                symbolic_class_tparams: &symbolic_ctp,
                                class_type_parameters: &classifier_header.type_parameters,
                                compact_headers,
                                resolved_annotations: &table.resolved_annotations,
                                compilation_id: table.compilation_id,
                                source_file: i as u32,
                                libraries: &*libraries,
                                owner_is_interface: classifier_flags.has(ClassFlags::INTERFACE),
                            },
                            c,
                            compact_classifier.map(|stub| stub.id),
                            d,
                            diags,
                        );
                    table.insert_class_sig(
                        internal,
                        ClassSig::with_declared_callable_headers(DeclaredCallableClassHeader {
                            internal,
                            stable_declaration: compact_classifier.map(|stub| stub.id),
                            source_file: i as u32,
                            source_decl: d,
                            visibility: classifier_visibility,
                            flags: classifier_flags,
                            methods: declared_methods,
                            declared_callable_order: classifier_callable_order.clone(),
                            member_ext_funs: declared_member_extensions,
                            companion_internal: direct_companion,
                            direct_supertypes: source_direct_supertypes
                                .get(&internal)
                                .cloned()
                                .unwrap_or_default()
                                .into(),
                            direct_supertype_arguments: classifier_header
                                .supertypes
                                .iter()
                                .map(|supertype| {
                                    supertype
                                        .targs
                                        .iter()
                                        .map(|argument| {
                                            ty_of_ref_silent(
                                                argument,
                                                &header_class_names,
                                                &symbolic_ctp,
                                            )
                                        })
                                        .collect()
                                })
                                .chain(classifier_header.base.iter().map(|base| {
                                    base.targs
                                        .iter()
                                        .map(|argument| {
                                            ty_of_ref_silent(
                                                argument,
                                                &header_class_names,
                                                &symbolic_ctp,
                                            )
                                        })
                                        .collect()
                                }))
                                .chain(
                                    classifier_flags
                                        .has(ClassFlags::ANNOTATION)
                                        .then_some(Vec::new()),
                                )
                                .collect(),
                            type_parameters: header_type_parameters,
                            type_parameter_extra_bounds: classifier_header
                                .type_parameters
                                .iter()
                                .map(|source| symbolic_ctp.extra_bounds_of(source))
                                .collect(),
                            captured_type_parameters: header_captured_type_parameters,
                        }),
                    );
                    if let Some(companion_decl) = c.companion {
                        if let (Some(companion_internal), Decl::Class(companion)) =
                            (direct_companion, file.decl(companion_decl))
                        {
                            if !table.classes.contains_key(&companion_internal) {
                                let compact_companion = compact_headers.and_then(|headers| {
                                    headers.stubs.iter().find(|stub| {
                                        stub.source.raw() == i as u32
                                            && stub.range == companion.span
                                            && stub.kind == crate::fir::DeclarationKind::Classifier
                                    })
                                });
                                let companion_header = match compact_headers {
                                    Some(headers) => {
                                        streamed_classifier_header_by_declaration(
                                            headers,
                                            compact_companion
                                                .expect("a production companion must have a compact identity")
                                                .id,
                                        )
                                        .expect("a production companion must have a compact header")
                                    }
                                    _ => legacy_classifier_header(companion),
                                };
                                let companion_tparams = TParams::from_decl_with(
                                    &companion_header.type_parameters,
                                    &companion_header.bounds,
                                    &|name| class_names.get(name),
                                );
                                let symbolic_companion_tparams = TParams::symbolic_from_decl_with(
                                    &companion_header.type_parameters,
                                    &companion_header.bounds,
                                    &|name| class_names.get(name),
                                )
                                .alpha_renamed_declaration(
                                    &companion_header.type_parameters,
                                    table.compilation_id,
                                    i as u32,
                                    companion.span.lo,
                                );
                                let (methods, extensions) = declared_member_callable_headers(
                                    &DeclaredMemberHeaderContext {
                                        file,
                                        classes: &class_names,
                                        class_tparams: &companion_tparams,
                                        symbolic_class_tparams: &symbolic_companion_tparams,
                                        class_type_parameters: &companion_header.type_parameters,
                                        compact_headers,
                                        resolved_annotations: &table.resolved_annotations,
                                        compilation_id: table.compilation_id,
                                        source_file: i as u32,
                                        libraries: &*libraries,
                                        owner_is_interface: compact_companion.map_or_else(
                                            || companion.is_interface(),
                                            |stub| {
                                                stub.flags
                                                    .has(crate::fir::DeclarationFlags::INTERFACE)
                                            },
                                        ),
                                    },
                                    companion,
                                    compact_companion.map(|stub| stub.id),
                                    companion_decl,
                                    diags,
                                );
                                let type_parameters = crate::types::TypeParameters::new(
                                    companion_header.type_parameters.clone(),
                                    companion_header
                                        .type_parameters
                                        .iter()
                                        .map(|parameter| {
                                            symbolic_companion_tparams
                                                .bound(parameter)
                                                .ty_param_bound()
                                                .unwrap_or_else(|| {
                                                    Ty::nullable(Ty::obj("kotlin/Any"))
                                                })
                                        })
                                        .collect(),
                                    companion_header.type_parameter_variances.clone(),
                                );
                                table.insert_class_sig(
                                    companion_internal,
                                    ClassSig::with_declared_callable_headers(
                                        DeclaredCallableClassHeader {
                                            internal: companion_internal,
                                            stable_declaration: compact_companion
                                                .map(|stub| stub.id),
                                            source_file: i as u32,
                                            source_decl: companion_decl,
                                            visibility: compact_companion
                                                .map(|stub| stub.visibility)
                                                .unwrap_or(companion.visibility),
                                            flags: compact_headers
                                                .zip(compact_companion)
                                                .map_or_else(
                                                    || source_class_flags(companion),
                                                    |(headers, stub)| {
                                                        streamed_source_class_flags(headers, stub)
                                                    },
                                                ),
                                            methods,
                                            declared_callable_order: compact_headers
                                                .zip(compact_companion)
                                                .map_or_else(
                                                    || source_declared_callable_order(companion),
                                                    |(headers, stub)| {
                                                        streamed_declared_callable_order(
                                                            headers, stub.id,
                                                        )
                                                    },
                                                ),
                                            member_ext_funs: extensions,
                                            companion_internal: None,
                                            direct_supertypes: Default::default(),
                                            direct_supertype_arguments: Vec::new(),
                                            type_parameters,
                                            type_parameter_extra_bounds: companion_header
                                                .type_parameters
                                                .iter()
                                                .map(|parameter| {
                                                    symbolic_companion_tparams
                                                        .extra_bounds_of(parameter)
                                                })
                                                .collect(),
                                            captured_type_parameters:
                                                crate::types::TypeParameters::default(),
                                        },
                                    ),
                                );
                            }
                        }
                    }
                    let member_this = Ty::obj_name(internal);
                    // Parser-hoisted local/anonymous classifiers are still body-local semantic
                    // units. Their inferred properties depend on lexical values and receiver rungs
                    // that exist only when Pass 2 reparses/checks the enclosing body, so they must
                    // never become module signature constraints. `Ty::Error` is the transient local
                    // classifier marker consumed by `check_class`, which replaces it with the exact
                    // checked result before publishing local FIR; it is not inserted into the
                    // pending-free module index.
                    let body_local_classifier = compact_classifier.map_or_else(
                        || {
                            file.is_local_declaration(d)
                                || legacy_anonymous_lexical_scope
                                    .is_some_and(|scope| scope.belongs_to_anonymous_subtree(d))
                        },
                        |stub| {
                            stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                                || stub
                                    .flags
                                    .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
                        },
                    );
                    let compact_body_properties = compact_headers.map(|headers| {
                        let owner = compact_classifier
                            .expect("a production class must have a stable identity")
                            .id;
                        let mut declarations = headers
                            .stubs
                            .iter()
                            .filter(|stub| {
                                stub.kind == crate::fir::DeclarationKind::Property
                                    && !stub
                                        .flags
                                        .has(crate::fir::DeclarationFlags::PROPERTY_PARAMETER)
                                    && headers
                                        .declarations
                                        .anchor(stub.id)
                                        .is_some_and(|anchor| anchor.owner == Some(owner))
                            })
                            .filter_map(|stub| {
                                headers
                                    .declarations
                                    .anchor(stub.id)
                                    .map(|anchor| (anchor.sibling, stub.id))
                            })
                            .collect::<Vec<_>>();
                        declarations.sort_by_key(|(sibling, _)| *sibling);
                        declarations
                            .into_iter()
                            .map(|(_, declaration)| declaration)
                            .collect::<Vec<_>>()
                    });
                    let body_property_count = compact_body_properties
                        .as_ref()
                        .map_or(c.body_props.len(), Vec::len);
                    for body_property_index in 0..body_property_count {
                        let bp = compact_headers
                            .is_none()
                            .then(|| &c.body_props[body_property_index]);
                        let source_property_index =
                            u32::try_from(classifier_header.primary_parameters.len())
                                .expect("too many constructor properties")
                                .checked_add(
                                    u32::try_from(body_property_index)
                                        .expect("too many body properties"),
                                )
                                .expect("too many class properties");
                        let property_header = match compact_headers {
                            Some(headers) => streamed_property_header_by_declaration(
                                headers,
                                compact_body_properties
                                    .as_ref()
                                    .and_then(|declarations| declarations.get(body_property_index))
                                    .copied()
                                    .expect(
                                        "a production member property must have a stable identity",
                                    ),
                            )
                            .expect("a production member property must have a compact header"),
                            None => legacy_property_header(
                                bp.expect("a legacy member property must have parser syntax"),
                            ),
                        };
                        if compact_headers.is_some() {
                            let flags = property_header.flags;
                            let owner_is_interface = classifier_flags.has(ClassFlags::INTERFACE);
                            validate_member_property_shape(
                                property_header.span,
                                owner_is_interface,
                                property_header.getter_declared,
                                flags.has(crate::fir::DeclarationFlags::CUSTOM_GETTER),
                                flags.has(crate::fir::DeclarationFlags::HAS_INITIALIZER),
                                flags.has(crate::fir::DeclarationFlags::DELEGATED),
                                flags.has(crate::fir::DeclarationFlags::LATEINIT),
                                flags.has(crate::fir::DeclarationFlags::ABSTRACT),
                                flags.has(crate::fir::DeclarationFlags::EXTERNAL),
                                flags.has(crate::fir::DeclarationFlags::EXPECT),
                                diags,
                            );
                            validate_context_property_shape(
                                property_header.span,
                                !property_header.context_parameters.is_empty(),
                                property_header.mutable,
                                flags.has(crate::fir::DeclarationFlags::CUSTOM_GETTER),
                                flags.has(crate::fir::DeclarationFlags::SETTER_HAS_BODY),
                                flags.has(crate::fir::DeclarationFlags::HAS_INITIALIZER),
                                flags.has(crate::fir::DeclarationFlags::DELEGATED),
                                flags.has(crate::fir::DeclarationFlags::LATEINIT),
                                flags.has(crate::fir::DeclarationFlags::CONST),
                                flags.has(crate::fir::DeclarationFlags::GETTER_READS_BACKING_FIELD),
                                owner_is_interface
                                    || flags.has(crate::fir::DeclarationFlags::ABSTRACT),
                                diags,
                            );
                            validate_explicit_backing_field_shape(
                                property_header.span,
                                flags.has(crate::fir::DeclarationFlags::EXPLICIT_BACKING_FIELD),
                                property_header.mutable,
                                flags.has(crate::fir::DeclarationFlags::OPEN),
                                flags.has(crate::fir::DeclarationFlags::CUSTOM_GETTER),
                                flags.has(crate::fir::DeclarationFlags::CUSTOM_SETTER),
                                flags.has(crate::fir::DeclarationFlags::DELEGATED),
                                flags.has(crate::fir::DeclarationFlags::CONST),
                                property_header.receiver.is_some(),
                                !property_header.context_parameters.is_empty(),
                                diags,
                            );
                        } else {
                            let bp = bp.expect("legacy property validation requires parser syntax");
                            validate_member_property(
                                bp,
                                classifier_flags.has(ClassFlags::INTERFACE),
                                diags,
                            );
                            validate_context_property(
                                bp,
                                classifier_flags.has(ClassFlags::INTERFACE) || bp.is_abstract,
                                diags,
                            );
                            validate_explicit_backing_field(bp, diags);
                        }
                        let resolve = |name: &str| class_names.get(name);
                        let btp = ctp.extended_with(
                            &property_header.type_parameters,
                            &property_header.bounds,
                            &resolve,
                        );
                        let symbolic_btp = symbolic_ctp
                            .symbolic_extended_with(
                                &property_header.type_parameters,
                                &property_header.bounds,
                                &resolve,
                            )
                            .alpha_renamed_declaration(
                                &property_header.type_parameters,
                                table.compilation_id,
                                i as u32,
                                property_header.span.lo,
                            );
                        let context_params = property_header
                            .context_parameters
                            .iter()
                            .map(|(_, parameter)| ty_of_ref(parameter, &class_names, &btp, diags))
                            .collect::<Vec<_>>();
                        let symbolic_context_params = property_header
                            .context_parameters
                            .iter()
                            .map(|(_, parameter)| {
                                ty_of_ref(parameter, &class_names, &symbolic_btp, diags)
                            })
                            .collect::<Vec<_>>();
                        let extension_receiver = property_header
                            .receiver
                            .as_ref()
                            .map(|receiver| ty_of_ref(receiver, &class_names, &btp, diags));
                        let mut property_scope = Vec::new();
                        property_scope.extend(
                            property_header
                                .context_parameters
                                .iter()
                                .zip(&context_params)
                                .filter(|((name, _), _)| name != "_")
                                .map(|((name, _), ty)| (name.clone(), *ty, false)),
                        );
                        if let Some(receiver) = extension_receiver {
                            property_scope.push(("this".to_string(), receiver, false));
                        }
                        property_scope.extend(init_scope.iter().cloned());
                        let property_ty = if compact_headers.is_some() {
                            property_header
                                .declared_type
                                .as_ref()
                                .map(|ty| ty_of_ref(ty, &class_names, &btp, diags))
                                .unwrap_or_else(|| {
                                    if body_local_classifier {
                                        Ty::Error
                                    } else if property_header.signature_inference.is_some() {
                                        // This is a temporary demandable candidate only. The compact
                                        // signature graph owns the expression and replaces this value
                                        // with a resolved type before the module index is published.
                                        Ty::Pending
                                    } else {
                                        Ty::Error
                                    }
                                })
                        } else if let Some(de) = bp
                            .expect("legacy property inference requires parser syntax")
                            .delegate
                        {
                            // A delegated member property: type = annotation, else the delegate's
                            // `getValue` return type.
                            match property_header.declared_type.as_ref() {
                                Some(r) => ty_of_ref(r, &class_names, &btp, diags),
                                None => {
                                    deferred_properties.push(DeferredProperty {
                                        key: DeclKey::member(i as u32, d.0, source_property_index),
                                        file_index: i as u32,
                                        name: property_header.name.clone(),
                                        span: property_header.span,
                                        delegate_of: Some(member_this),
                                        backing_field: false,
                                        expression: Some(de),
                                        scope: property_scope.clone(),
                                        kind: DeferredKind::Member(internal),
                                    });
                                    Ty::Pending
                                }
                            }
                        } else {
                            let bp = bp.expect("legacy property inference requires parser syntax");
                            match (property_header.declared_type.as_ref(), &bp.getter) {
                                (Some(r), _) => ty_of_ref(r, &class_names, &btp, diags),
                                (None, Some(FunBody::Expr(g))) => {
                                    let inferred = if body_local_classifier {
                                        Ty::Error
                                    } else {
                                        Ty::Pending
                                    };
                                    if inferred == Ty::Pending && bp.receiver.is_none() {
                                        deferred_properties.push(DeferredProperty {
                                            key: DeclKey::member(
                                                i as u32,
                                                d.0,
                                                source_property_index,
                                            ),
                                            file_index: i as u32,
                                            name: bp.name.clone(),
                                            span: bp.span,
                                            delegate_of: None,
                                            backing_field: false,
                                            expression: Some(*g),
                                            scope: property_scope.clone(),
                                            kind: DeferredKind::Member(internal),
                                        });
                                    }
                                    inferred
                                }
                                (None, _) => match bp.init {
                                    Some(expression) => {
                                        let inferred = if body_local_classifier {
                                            Ty::Error
                                        } else {
                                            Ty::Pending
                                        };
                                        if inferred == Ty::Pending && bp.receiver.is_none() {
                                            deferred_properties.push(DeferredProperty {
                                                key: DeclKey::member(
                                                    i as u32,
                                                    d.0,
                                                    source_property_index,
                                                ),
                                                file_index: i as u32,
                                                name: bp.name.clone(),
                                                span: bp.span,
                                                delegate_of: None,
                                                backing_field: false,
                                                expression: Some(expression),
                                                scope: property_scope.clone(),
                                                kind: DeferredKind::Member(internal),
                                            });
                                        }
                                        inferred
                                    }
                                    None => Ty::Error,
                                },
                            }
                        };
                        let property_ty = if property_header.declared_type.is_none() {
                            inferred_declaration_ty(property_ty)
                        } else {
                            property_ty
                        };
                        let storage_ty = if compact_headers.is_some() {
                            property_header
                                .flags
                                .has(crate::fir::DeclarationFlags::EXPLICIT_BACKING_FIELD)
                                .then(|| {
                                    property_header
                                        .backing_field_type
                                        .as_ref()
                                        .map(|ty| ty_of_ref(ty, &class_names, &btp, diags))
                                        .or_else(|| {
                                            (property_header.signature_inference
                                                == Some(
                                                    crate::fir::InferredSignatureKind::BackingFieldInitializer,
                                                ))
                                            .then_some(Ty::Pending)
                                        })
                                        .unwrap_or(Ty::Error)
                                })
                        } else {
                            let bp =
                                bp.expect("legacy backing-field inference requires parser syntax");
                            bp.explicit_backing_field.as_ref().map(|_| {
                                property_header
                                    .backing_field_type
                                    .as_ref()
                                    .map(|ty| ty_of_ref(ty, &class_names, &btp, diags))
                                    .or_else(|| {
                                        // Inferred from the field's own initializer, which needs the
                                        // class's context; resolved after the walk like every other
                                        // implicitly-typed body.
                                        bp.init.map(|init| {
                                            deferred_properties.push(DeferredProperty {
                                                key: DeclKey::member(
                                                    i as u32,
                                                    d.0,
                                                    source_property_index,
                                                ),
                                                file_index: i as u32,
                                                name: bp.name.clone(),
                                                span: bp.span,
                                                delegate_of: None,
                                                backing_field: true,
                                                expression: Some(init),
                                                scope: property_scope.clone(),
                                                kind: DeferredKind::Member(internal),
                                            });
                                            Ty::Pending
                                        })
                                    })
                                    .unwrap_or(Ty::Error)
                            })
                        };
                        if storage_ty == Some(Ty::Error) {
                            diags.error(
                                property_header.span,
                                format!(
                                    "cannot infer the backing field type of '{}'; add an explicit type",
                                    property_header.name
                                ),
                            );
                        }
                        // A field whose type the engine has not resolved yet cannot be compared
                        // against the property's; the authoritative check does it once both are
                        // known.
                        if let Some(field_ty) = storage_ty.filter(|ty| *ty != Ty::Pending) {
                            let matcher = table.source_constructor_matcher_with(&*libraries);
                            if !crate::assignable::is_subtype(
                                &crate::assignable::TyCtx::new(),
                                &matcher,
                                field_ty,
                                property_ty,
                            ) {
                                diags.error(
                                    property_header.span,
                                    format!(
                                        "backing field type of '{}' is '{}', which is not a subtype of its property type '{}'.",
                                        property_header.name,
                                        field_ty.source_name(),
                                        property_ty.source_name(),
                                    ),
                                );
                            }
                        }
                        if let (Some(receiver_ty), Some(receiver)) =
                            (extension_receiver, property_header.receiver.as_ref())
                        {
                            let key = (receiver_ty.erased_recv(), property_header.name.clone());
                            if !member_ext_keys.insert(key) {
                                diags.error(
                                    property_header.span,
                                    format!(
                                        "krusty: conflicting member extension property '{}'",
                                        property_header.name
                                    ),
                                );
                            }
                            member_ext_props
                                .entry(property_header.name.clone())
                                .or_insert_with(Vec::new)
                                .push(MemberExtPropSig {
                                    receiver: ty_of_ref(
                                        receiver,
                                        &class_names,
                                        &symbolic_btp,
                                        diags,
                                    ),
                                    ret: property_header
                                        .declared_type
                                        .as_ref()
                                        .map(|ret| {
                                            ty_of_ref(ret, &class_names, &symbolic_btp, diags)
                                        })
                                        .unwrap_or(property_ty),
                                    context_params: symbolic_context_params,
                                    type_params: property_header
                                        .type_parameters
                                        .iter()
                                        .map(|source| {
                                            symbolic_btp
                                                .bound(source)
                                                .ty_param_name()
                                                .unwrap_or(source)
                                                .to_string()
                                        })
                                        .collect(),
                                    type_param_bounds: property_header
                                        .type_parameters
                                        .iter()
                                        .map(|parameter| {
                                            symbolic_btp
                                                .bound(parameter)
                                                .ty_param_bound()
                                                .unwrap_or_else(|| Ty::obj("kotlin/Any"))
                                        })
                                        .collect::<Vec<_>>(),
                                    is_var: property_header.mutable,
                                    visibility: property_header.visibility,
                                    setter_visibility: property_header
                                        .mutable
                                        .then_some(property_header.setter_visibility),
                                    annotations: declaration_annotation_identities(
                                        compact_headers,
                                        property_header.declaration,
                                        &table.resolved_annotations,
                                        || {
                                            resolved_header_annotation_identities(
                                                &property_header.annotations,
                                                &class_names,
                                            )
                                        },
                                    ),
                                    stable_declaration: property_header.declaration,
                                    source_member: compact_headers.is_none().then_some(
                                        crate::libraries::SourceMember::ClassProperty {
                                            file: i as u32,
                                            owner: d.0,
                                            property: source_property_index,
                                        },
                                    ),
                                    getter: None,
                                    setter: None,
                                });
                            continue;
                        }
                        let resolved_storage_ty = storage_ty.unwrap_or(property_ty);
                        if property_header.context_parameters.is_empty() {
                            props.push((
                                property_header.name.clone(),
                                resolved_storage_ty,
                                property_header.mutable,
                            ));
                        }
                        let declared_property = DeclaredPropertySig {
                            ty: property_ty,
                            storage_ty,
                            visibility: property_header.visibility,
                            source_visible: true,
                            is_const: property_header
                                .flags
                                .has(crate::fir::DeclarationFlags::CONST),
                            annotations: declaration_annotation_identities(
                                compact_headers,
                                property_header.declaration,
                                &table.resolved_annotations,
                                || {
                                    resolved_header_annotation_identities(
                                        &property_header.annotations,
                                        &class_names,
                                    )
                                },
                            ),
                            getter_name: property_getter_name(&property_header.name),
                            setter_name: property_header
                                .mutable
                                .then(|| property_setter_name(&property_header.name)),
                            setter_parameter_name: property_header.setter_parameter_name.clone(),
                            setter_visibility: property_header
                                .mutable
                                .then_some(property_header.setter_visibility),
                            has_custom_getter: property_header
                                .flags
                                .has(crate::fir::DeclarationFlags::CUSTOM_GETTER)
                                || property_header
                                    .flags
                                    .has(crate::fir::DeclarationFlags::DELEGATED),
                            is_abstract: property_header
                                .flags
                                .has(crate::fir::DeclarationFlags::ABSTRACT),
                            // An abstract property is necessarily overridable even when its
                            // source omitted the redundant `open` modifier.
                            is_open: property_header
                                .flags
                                .has(crate::fir::DeclarationFlags::OPEN)
                                || property_header
                                    .flags
                                    .has(crate::fir::DeclarationFlags::ABSTRACT),
                            context_params,
                            source_member: compact_headers.is_none().then_some(
                                crate::libraries::SourceMember::ClassProperty {
                                    file: i as u32,
                                    owner: d.0,
                                    property: source_property_index,
                                },
                            ),
                            stable_declaration: property_header.declaration,
                        };
                        if declared_property.context_params.is_empty() {
                            declared_props.insert(property_header.name.clone(), declared_property);
                        } else {
                            contextual_props
                                .entry(property_header.name.clone())
                                .or_default()
                                .push(declared_property);
                        }
                        if property_header.context_parameters.is_empty() {
                            init_scope.push((
                                property_header.name.clone(),
                                resolved_storage_ty,
                                property_header.mutable,
                            ));
                        }
                    }
                    let mut methods: MethodMap = MethodMap::new();
                    let mut declared_callable_order = classifier_callable_order;
                    let mut member_ext_funs: HashMap<String, Vec<MemberExtFunSig>> = HashMap::new();
                    let mut source_methods = Vec::new();
                    let mut generic_methods: HashMap<String, Vec<GenericMethod>> = HashMap::new();
                    let compact_member_callables = compact_headers.map(|headers| {
                        streamed_owned_callable_declarations(
                            headers,
                            compact_classifier
                                .expect("a production classifier must have a stable identity")
                                .id,
                        )
                    });
                    let member_callable_count = compact_member_callables
                        .as_ref()
                        .map_or_else(|| c.methods.len(), Vec::len);
                    for method_index in 0..member_callable_count {
                        let method = compact_headers.is_none().then(|| &c.methods[method_index]);
                        let method_header = match compact_headers {
                            Some(headers) => streamed_callable_header_by_declaration(
                                headers,
                                compact_member_callables
                                    .as_ref()
                                    .and_then(|declarations| declarations.get(method_index))
                                    .copied()
                                    .expect(
                                        "a production member function must have a stable identity",
                                    ),
                            )
                            .expect("a production member function must have a compact header"),
                            None => legacy_callable_header(
                                method.expect("a legacy member function must have parser syntax"),
                            ),
                        };
                        let mtp = ctp.extended_with(
                            &method_header.type_parameters,
                            &method_header.bounds,
                            &|n| class_names.get(n),
                        );
                        // A `vararg` parameter's runtime type is `Array<elem>` (mirrors the
                        // top-level-function path) — without this a member `vararg s: String`
                        // erases to a single `String` and a call passes the element where the
                        // `String[]` is expected (a `ClassCastException`).
                        let ret = method_header
                            .explicit_result
                            .as_ref()
                            .map(|r| ty_of_ref(r, &class_names, &mtp, diags))
                            .unwrap_or_else(|| {
                                if method_header.signature_inference.is_some() {
                                    // Inferred member results belong exclusively to the declaration's
                                    // compact signature constraint. The former walk typed this body
                                    // against a half-built class table and discarded the answer before
                                    // returning `Pending`; besides retaining ordinary syntax longer,
                                    // that redundant traversal could still populate stale resolver
                                    // caches. Keep only the provisional candidate shape here.
                                    return Ty::Pending;
                                }
                                Ty::Unit
                            });
                        let mut signature = if let Some(headers) = compact_headers {
                            member_signature_from_header(
                                &method_header,
                                MemberSignatureFacts {
                                    annotations: compact_declaration_annotation_identities(
                                        headers,
                                        method_header.declaration,
                                        &table.resolved_annotations,
                                    ),
                                    source_file: i as u32,
                                    source_member: None,
                                    param_default_values: vec![
                                        None;
                                        method_header.parameters.len()
                                    ],
                                },
                                ret,
                                &class_names,
                                &mtp,
                                diags,
                            )
                        } else {
                            let source_member = crate::libraries::SourceMember::Class {
                                file: i as u32,
                                owner: d.0,
                                method: method_index as u32,
                            };
                            legacy_member_signature_from_header(
                                file,
                                method.expect("a legacy member function must have parser syntax"),
                                &method_header,
                                ret,
                                &class_names,
                                &mtp,
                                i as u32,
                                source_member,
                                &*libraries,
                                true,
                                diags,
                            )
                        };
                        if classifier_flags.has(ClassFlags::INTERFACE)
                            && method.is_some_and(|method| matches!(method.body, FunBody::None))
                        {
                            signature.flags = signature.flags.with_is_abstract(true);
                        }
                        let has_generic_shape = !method_header.type_parameters.is_empty()
                            || !captured_semantic_tparam_names.is_empty()
                            || !classifier_header.type_parameters.is_empty();
                        if has_generic_shape {
                            let symbolic_mtp = symbolic_ctp
                                .symbolic_extended_with(
                                    &method_header.type_parameters,
                                    &method_header.bounds,
                                    &|name| class_names.get(name),
                                )
                                .alpha_renamed_declaration(
                                    &method_header.type_parameters,
                                    table.compilation_id,
                                    i as u32,
                                    method_header.signature_start,
                                );
                            let ret_shape = method_header
                                .explicit_result
                                .as_ref()
                                .map(|ret| ty_of_ref(ret, &class_names, &symbolic_mtp, diags))
                                .unwrap_or(signature.ret);
                            signature.generic_sig = Some(source_generic_signature_from_header(
                                &method_header,
                                &class_names,
                                &symbolic_mtp,
                                ret_shape,
                                if compact_headers.is_some() {
                                    // The compact signature graph owns inferred member results too.
                                    // This provisional legacy symbol view must not reopen a body
                                    // whose arena was released before signature collection.
                                    None
                                } else {
                                    inferred_return_type_parameter(
                                        file,
                                        method.expect(
                                            "a legacy member function must have parser syntax",
                                        ),
                                    )
                                },
                                diags,
                            ));
                            if method_header.receiver.is_none() {
                                generic_methods
                                    .entry(method_header.name.clone())
                                    .or_default()
                                    .push(GenericMethod {
                                        signature: signature
                                            .generic_sig
                                            .as_ref()
                                            .expect("a generic member must retain its declaration signature")
                                            .clone(),
                                        params: signature.params.clone(),
                                        param_names: method_header
                                            .parameters
                                            .iter()
                                            .map(|parameter| parameter.name.clone())
                                            .collect(),
                                    });
                            }
                        }
                        let value_operand_slots = if compact_headers.is_some() {
                            generic_value_operand_slots_from_header(
                                &method_header,
                                &classifier_header.type_parameters,
                            )
                        } else {
                            generic_value_operand_slots(
                                method.expect("a legacy member function must have parser syntax"),
                                &classifier_header.type_parameters,
                            )
                        };
                        if !value_operand_slots.is_empty() {
                            let physical_receiver = method_header
                                .receiver
                                .as_ref()
                                .map(|receiver| ty_of_ref(receiver, &class_names, &mtp, diags));
                            table
                                .source_generic_member_value_operand_slots
                                .entry(internal)
                                .or_default()
                                .entry(method_header.name.clone())
                                .or_default()
                                .push((
                                    physical_receiver,
                                    signature.params.clone(),
                                    value_operand_slots,
                                ));
                        }
                        let extension_receiver = method_header
                            .receiver
                            .as_ref()
                            .map(|receiver| ty_of_ref(receiver, &class_names, &mtp, diags));
                        source_methods.push(SourceMethodPlan {
                            signature: signature.clone(),
                            extension_receiver,
                        });
                        if let Some(receiver_ty) = extension_receiver {
                            member_ext_funs
                                .entry(method_header.name.clone())
                                .or_default()
                                .push(MemberExtFunSig {
                                    receiver_ty,
                                    physical_receiver: receiver_ty,
                                    physical_params: signature.params.clone(),
                                    signature,
                                    physical_name: method_header.name.clone(),
                                    external_identity: None,
                                    external_default_provider: None,
                                    declared_ret: None,
                                    inline_body_plan: None,
                                });
                        } else {
                            methods
                                .entry(method_header.name.clone())
                                .or_default()
                                .push(signature);
                        }
                    }
                    // Kotlin's generated object overrides are ordinary semantic declarations. Give
                    // them the same stable identity and callable shape as written members; common IR
                    // alone owns their synthesized bodies.
                    if classifier_is_data {
                        for (name, params, ret) in [
                            ("toString", Vec::new(), Ty::String),
                            ("hashCode", Vec::new(), Ty::Int),
                            (
                                "equals",
                                vec![Ty::nullable(Ty::obj("kotlin/Any"))],
                                Ty::Boolean,
                            ),
                        ] {
                            let already_declared = methods.get(name).is_some_and(|overloads| {
                                overloads.iter().any(|signature| {
                                    signature.is_override()
                                        && signature.params.len() == params.len()
                                })
                            });
                            if already_declared {
                                continue;
                            }
                            if !declared_callable_order
                                .iter()
                                .any(|declared| declared == name)
                            {
                                declared_callable_order.push(name.to_string());
                            }
                            let parameter_count = params.len();
                            methods.entry(name.into()).or_default().push(Signature {
                                params,
                                ret,
                                generic_sig: None,
                                projected_return_hazard: false,
                                flags: SigFlags::default()
                                    .with_is_operator(name == "equals")
                                    .with_is_override(true)
                                    .with_is_final(true),
                                annotations: Vec::new(),
                                equality_bound: (name == "equals").then(|| Ty::obj_name(internal)),
                                vararg_index: None,
                                required: parameter_count,
                                param_defaults: vec![false; parameter_count],
                                exact_params: vec![false; parameter_count],
                                no_infer_params: vec![false; parameter_count],
                                implicit_integer_coercion: vec![false; parameter_count],
                                param_default_values: vec![None; parameter_count],
                                param_names: if name == "equals" {
                                    vec!["other".into()]
                                } else {
                                    Vec::new()
                                },
                                lambda_param_types: vec![Vec::new(); parameter_count],
                                lambda_recv: vec![false; parameter_count],
                                visibility: Visibility::Public,
                                context_count: 0,
                                source_decl: None,
                                stable_declaration: compact_generated_member_declaration(
                                    compact_headers,
                                    compact_classifier.map(|stub| stub.id),
                                    name,
                                ),
                                source_file: None,
                                source_member: None,
                                source_receiver: None,
                                package: String::new(),
                                contract: None,
                                plugin_expression: None,
                            });
                        }
                    }
                    // Only a `data class` synthesizes componentN/copy. A `data object` has no
                    // destructurable constructor state and Kotlin deliberately declares neither;
                    // a user-written `copy` on one must remain the sole overload.
                    if classifier_is_data && !classifier_flags.has(ClassFlags::OBJECT) {
                        let self_ty = Ty::obj_name(internal);
                        let self_shape = Ty::obj_args_name(
                            internal,
                            &classifier_header
                                .type_parameters
                                .iter()
                                .map(|parameter| symbolic_ctp.bound(parameter))
                                .collect::<Vec<_>>(),
                        );
                        let data_properties = classifier_header
                            .primary_parameters
                            .iter()
                            .zip(&ctor_params)
                            .zip(&ctor_param_shapes)
                            .filter(|((property, _), _)| property.is_property)
                            .collect::<Vec<_>>();
                        for (i, ((_, ty), (shape, _))) in data_properties.iter().enumerate() {
                            let generated_name = format!("component{}", i + 1);
                            declared_callable_order.push(generated_name.clone());
                            methods.insert(
                                generated_name.clone(),
                                vec![Signature {
                                    params: vec![],
                                    ret: **ty,
                                    generic_sig: Some(GenericSig {
                                        formals: Vec::new(),
                                        formal_bounds: Vec::new(),
                                        receiver: None,
                                        params: Vec::new(),
                                        ret: *shape,
                                        return_policy: GenericReturnPolicy::Exact,
                                    }),
                                    projected_return_hazard: false,
                                    flags: SigFlags::default()
                                        .with_vararg(false)
                                        .with_is_inline(false)
                                        .with_is_operator(true)
                                        .with_is_override(false)
                                        .with_is_final(true)
                                        .with_is_suspend(false),
                                    annotations: Vec::new(),
                                    equality_bound: None,
                                    vararg_index: None,
                                    required: 0,
                                    param_defaults: Vec::new(),
                                    exact_params: Vec::new(),
                                    no_infer_params: Vec::new(),
                                    implicit_integer_coercion: Vec::new(),
                                    param_default_values: Vec::new(),
                                    param_names: Vec::new(),
                                    lambda_param_types: Vec::new(),
                                    lambda_recv: Vec::new(),
                                    visibility: Visibility::Public,
                                    context_count: 0,
                                    source_decl: None,
                                    stable_declaration: compact_generated_member_declaration(
                                        compact_headers,
                                        compact_classifier.map(|stub| stub.id),
                                        &generated_name,
                                    ),
                                    source_file: None,
                                    source_member: None,
                                    source_receiver: None,
                                    package: String::new(),
                                    contract: None,
                                    plugin_expression: None,
                                }],
                            );
                        }
                        // Every `copy` parameter has a default (the receiver's property) — so `required`
                        // is 0 and any subset may be passed, by name or position.
                        declared_callable_order.push("copy".to_string());
                        methods.insert(
                            "copy".into(),
                            vec![Signature {
                                params: data_properties.iter().map(|((_, ty), _)| **ty).collect(),
                                ret: self_ty,
                                generic_sig: Some(GenericSig {
                                    formals: Vec::new(),
                                    formal_bounds: Vec::new(),
                                    receiver: None,
                                    params: data_properties
                                        .iter()
                                        .map(|(_, (shape, _))| *shape)
                                        .collect(),
                                    ret: self_shape,
                                    return_policy: GenericReturnPolicy::Exact,
                                }),
                                projected_return_hazard: false,
                                flags: SigFlags::default(),
                                annotations: Vec::new(),
                                equality_bound: None,
                                vararg_index: None,
                                required: 0,
                                param_defaults: vec![true; data_properties.len()],
                                exact_params: vec![false; data_properties.len()],
                                no_infer_params: vec![false; data_properties.len()],
                                implicit_integer_coercion: vec![false; data_properties.len()],
                                param_default_values: Vec::new(),
                                param_names: data_properties
                                    .iter()
                                    .map(|((property, _), _)| property.name.clone())
                                    .collect(),
                                lambda_param_types: Vec::new(),
                                lambda_recv: Vec::new(),
                                visibility: Visibility::Public,
                                context_count: 0,
                                source_decl: None,
                                stable_declaration: compact_generated_member_declaration(
                                    compact_headers,
                                    compact_classifier.map(|stub| stub.id),
                                    "copy",
                                ),
                                source_file: None,
                                source_member: None,
                                source_receiver: None,
                                package: String::new(),
                                contract: None,
                                plugin_expression: None,
                            }],
                        );
                    }
                    if classifier_is_enum {
                        table.enums.insert(
                            internal,
                            c.enum_entries.iter().map(|e| e.name.clone()).collect(),
                        );
                    }
                    // An unresolved supertype is diagnosed at its own source span and is never emitted.
                    let super_span = |name: &str| {
                        if let Some(base) = classifier_header
                            .base
                            .as_ref()
                            .filter(|base| base.name == name)
                        {
                            return base.span;
                        }
                        classifier_header
                            .supertypes
                            .iter()
                            .find(|t| t.name == name)
                            .map(|t| t.span)
                            .unwrap_or(c.span)
                    };
                    let mut resolve_super = |s: &str| -> String {
                        let resolved =
                            declared_supertype_name(c, s, &header_class_names, &lexical_inheritors)
                                .map(TypeName::render)
                                // An erased type parameter used as a supertype (degenerate) stays as-is.
                                .or_else(|| ctp.contains(s).then(|| s.to_string()));
                        match resolved {
                            Some(internal) => internal,
                            None => {
                                if !compact_local_header {
                                    let segment = class_names.unresolved_segment(s);
                                    diags.error(
                                        super_span(s),
                                        format!("unresolved reference '{segment}'."),
                                    );
                                }
                                s.to_string()
                            }
                        }
                    };
                    // The parser can promote only a SAME-FILE base; at this all-files signature pass,
                    // classify an other-file module declaration from the bootstrap index and otherwise
                    // ask the library source. Both origins feed the same `super_internal` field, so later
                    // checking/lowering never needs separate file/module/classpath branches.
                    let parenless_base = if c.primary_ctor_annotations.is_some()
                        || classifier_header.base.is_some()
                        || !c.secondary_ctors.iter().any(|constructor| {
                            matches!(
                                constructor.delegation,
                                crate::ast::CtorDelegation::Super(_)
                                    | crate::ast::CtorDelegation::None
                            )
                        }) {
                        None
                    } else {
                        classifier_header
                            .supertypes
                            .iter()
                            .find(|supertype| {
                                declared_supertype_name(
                                    c,
                                    &supertype.name,
                                    &header_class_names,
                                    &lexical_inheritors,
                                )
                                .is_some_and(|internal| {
                                    user_base_classes.contains(&internal)
                                        || libraries
                                            .classifier(internal)
                                            .is_some_and(|ty| !ty.is_interface() && !ty.is_object())
                                })
                            })
                            .map(|supertype| supertype.name.clone())
                    };
                    let mut interfaces: Vec<String> = classifier_header
                        .supertypes
                        .iter()
                        // A function supertype through the numbered semantic classifier contributes
                        // both its nominal `kotlin/FunctionN` edge and its exact callable shape. Only
                        // the unmaterialized `<fun>` marker used by suspend/big-arity shapes lacks a
                        // nominal classifier and must stay out of hierarchy traversal.
                        .filter(|t| t.name != "<fun>")
                        .filter(|t| parenless_base.as_deref() != Some(t.name.as_str()))
                        .map(|t| resolve_super(&t.name))
                        .collect();
                    interfaces.extend(implicit_source_supertypes(c).map(TypeName::render));
                    let super_internal = classifier_header
                        .base
                        .as_ref()
                        .map(|base| base.name.as_str())
                        .or(parenless_base.as_deref())
                        .map(&mut resolve_super)
                        .or_else(|| classifier_is_enum.then(|| "kotlin/Enum".to_string()));
                    let resolved_annotations: Vec<_> = c
                        .annotations
                        .iter()
                        .filter_map(|annotation| table.resolved_annotation(i as u32, annotation))
                        .collect();
                    // A class-valued annotation argument is resolved here, next to the annotation's
                    // own name and through the same classifier rules. Another file of this module
                    // reads the result from the stable index; it never sees this declaration's
                    // syntax, and recovering the identity from a spelling later is not available to
                    // it.
                    let resolved_annotation_class_arguments: Vec<(u32, TypeName)> = compact_headers
                        .zip(compact_classifier)
                        .map(|(headers, stub)| {
                            streamed_declaration_annotation_class_literals(headers, stub.id)
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|(ordinal, path)| {
                            Some((ordinal, class_names.classifier_binding(&path).ok()?))
                        })
                        .collect();
                    let semantic_tparam_names = classifier_header
                        .type_parameters
                        .iter()
                        .map(|source| {
                            symbolic_ctp
                                .bound(source)
                                .ty_param_name()
                                .unwrap_or(source)
                                .to_string()
                        })
                        .collect::<Vec<_>>();
                    let tparam_bounds = classifier_header
                        .type_parameters
                        .iter()
                        .map(|source| {
                            symbolic_ctp
                                .bound(source)
                                .ty_param_bound()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                        })
                        .collect::<Vec<_>>();
                    let class_type_parameters = crate::types::TypeParameters::new(
                        semantic_tparam_names.clone(),
                        tparam_bounds.clone(),
                        classifier_header.type_parameter_variances.clone(),
                    );
                    let captured_bounds = captured_parameters
                        .iter()
                        .map(|(_, bound)| *bound)
                        .collect::<Vec<_>>();
                    let captured_type_parameters = crate::types::TypeParameters::invariant(
                        captured_semantic_tparam_names,
                        captured_bounds,
                    );
                    let classifier_semantic_tparam_names = semantic_tparam_names
                        .iter()
                        .chain(captured_type_parameters.type_params.iter())
                        .cloned()
                        .collect::<Vec<_>>();
                    let frontend_plugin_context = crate::plugins::FrontendClassContext {
                        classifier: internal,
                        companion: direct_companion,
                        kind: frontend_class_kind(classifier_flags, classifier_is_enum),
                        is_sealed: classifier_flags.has(ClassFlags::SEALED),
                        type_parameters: &class_type_parameters,
                        annotations: &resolved_annotations,
                        annotation_class_arguments: &resolved_annotation_class_arguments,
                    };
                    let mut contributed_members = Vec::new();
                    frontend_plugins.generate_frontend_declarations(
                        &frontend_plugin_context,
                        &mut contributed_members,
                    );
                    let mut generated_nested_classifiers = Vec::new();
                    frontend_plugins.publish_frontend_generated_classifiers(
                        &frontend_plugin_context,
                        &mut generated_nested_classifiers,
                    );
                    let mut contributed_companion_methods = MethodMap::new();
                    let mut contributed_companion_order = Vec::new();
                    for member in contributed_members {
                        let required = member.params.len();
                        assert_eq!(
                            member.param_names.len(),
                            required,
                            "a frontend plugin declaration publishes one identity per parameter"
                        );
                        let signature = Signature {
                            params: member.params,
                            ret: member.ret,
                            generic_sig: member.generic_sig,
                            projected_return_hazard: false,
                            flags: SigFlags::default().with_is_final(true),
                            annotations: Vec::new(),
                            equality_bound: None,
                            vararg_index: None,
                            required,
                            param_defaults: vec![],
                            exact_params: vec![],
                            no_infer_params: vec![],
                            implicit_integer_coercion: vec![],
                            param_default_values: Vec::new(),
                            param_names: member.param_names,
                            lambda_param_types: vec![],
                            lambda_recv: vec![],
                            visibility: Visibility::Public,
                            context_count: 0,
                            source_decl: None,
                            stable_declaration: None,
                            source_file: None,
                            source_member: None,
                            source_receiver: None,
                            package: String::new(),
                            contract: None,
                            plugin_expression: member.plugin_expression,
                        };
                        let name = member.name;
                        match member.owner {
                            crate::plugins::FrontendCallableOwner::Classifier => {
                                if !declared_callable_order.contains(&name) {
                                    declared_callable_order.push(name.clone());
                                }
                                methods.entry(name).or_default().push(signature)
                            }
                            crate::plugins::FrontendCallableOwner::Companion => {
                                if !contributed_companion_order.contains(&name) {
                                    contributed_companion_order.push(name.clone());
                                }
                                contributed_companion_methods
                                    .entry(name)
                                    .or_default()
                                    .push(signature)
                            }
                        }
                    }
                    let constants = c
                        .body_props
                        .iter()
                        .filter(|property| {
                            classifier_flags.has(ClassFlags::OBJECT) && property.is_const
                        })
                        .filter_map(|property| {
                            let ty = declared_props.get(&property.name)?.ty;
                            let value = source_literal_constant(file, property.init?, ty)?;
                            Some((property.name.clone(), value))
                        })
                        .collect::<HashMap<_, _>>();
                    let lateinit_props: std::collections::HashSet<String> = c
                        .body_props
                        .iter()
                        .filter(|p| p.is_lateinit)
                        .map(|p| p.name.clone())
                        .collect();
                    // Record which properties are declared as a bare type parameter, so a read on a
                    // generic instantiation can substitute the corresponding type argument. A nullable
                    // parameter (`T?`) has its own direct-shape table because nullability wraps the
                    // declaration parameter rather than the use-site argument.
                    let tparam_index = |r: &TypeRef| -> Option<(usize, bool)> {
                        if r.nullable() || !r.targs.is_empty() || r.arg.is_some() {
                            return None;
                        }
                        classifier_header
                            .type_parameters
                            .iter()
                            .position(|t| *t == r.name)
                            .map(|index| (index, r.definitely_non_null()))
                    };
                    // Direct `T?` remains distinct from bare `T` for member selection, but both retain
                    // their complete symbolic declaration shape below. Storage erasure and the
                    // nullable-scalar boxing boundary are backend work, never a reason to widen a
                    // frontend signature to the parameter's upper bound.
                    let is_direct_nullable_tparam = |r: &TypeRef| {
                        r.nullable()
                            && r.targs.is_empty()
                            && r.arg.is_none()
                            && classifier_header
                                .type_parameters
                                .iter()
                                .any(|parameter| parameter == &r.name)
                    };
                    let mut generic_props: HashMap<String, (usize, bool)> = HashMap::new();
                    let mut nullable_tparam_props: HashMap<String, usize> = HashMap::new();
                    for parameter in classifier_header
                        .primary_parameters
                        .iter()
                        .filter(|parameter| parameter.is_property && !parameter.is_vararg)
                    {
                        if let Some(index) = tparam_index(&parameter.ty) {
                            generic_props.insert(parameter.name.clone(), index);
                        }
                        if is_direct_nullable_tparam(&parameter.ty) {
                            if let Some(index) = classifier_header
                                .type_parameters
                                .iter()
                                .position(|name| *name == parameter.ty.name)
                            {
                                nullable_tparam_props.insert(parameter.name.clone(), index);
                            }
                        }
                    }
                    for bp in &c.body_props {
                        if let Some(r) = &bp.ty {
                            if let Some(i) = tparam_index(r) {
                                generic_props.insert(bp.name.clone(), i);
                            }
                            if is_direct_nullable_tparam(r) {
                                if let Some(i) = classifier_header
                                    .type_parameters
                                    .iter()
                                    .position(|t| *t == r.name)
                                {
                                    nullable_tparam_props.insert(bp.name.clone(), i);
                                }
                            }
                        }
                    }
                    let mut generic_property_shapes = HashMap::new();
                    for (property, &(shape, _)) in classifier_header
                        .primary_parameters
                        .iter()
                        .zip(&ctor_param_shapes)
                        .filter(|(property, _)| property.is_property)
                    {
                        if ty_mentions_param(shape, &classifier_semantic_tparam_names) {
                            generic_property_shapes.insert(property.name.clone(), shape);
                        }
                    }
                    for property in c
                        .body_props
                        .iter()
                        .filter(|property| property.receiver.is_none())
                    {
                        if let Some(type_ref) = &property.ty {
                            let shape = ty_of_ref_silent(type_ref, &class_names, &symbolic_ctp);
                            if ty_mentions_param(shape, &classifier_semantic_tparam_names) {
                                generic_property_shapes.insert(property.name.clone(), shape);
                            }
                        }
                    }
                    let secondary_constructors = c
                        .secondary_ctors
                        .iter()
                        .enumerate()
                        .map(|(index, constructor)| {
                            let parameters = match compact_headers {
                                Some(headers)
                                    if headers.has_headers(crate::fir::SourceFileId::from_raw(
                                        i as u32,
                                    )) =>
                                {
                                    streamed_secondary_constructor_parameters(
                                        headers,
                                        compact_classifier
                                            .expect("a production classifier has a stable identity")
                                            .id,
                                        u32::try_from(index + 1)
                                            .expect("too many secondary constructors"),
                                    )
                                    .expect(
                                        "a production secondary constructor must have a compact header",
                                    )
                                }
                                _ => constructor
                                    .params
                                    .iter()
                                    .map(|parameter| StreamedCallableParameter {
                                        name: parameter.name.clone(),
                                        ty: parameter.ty.clone(),
                                        is_vararg: parameter.is_vararg,
                                        has_default: parameter.default.is_some(),
                                        annotations: parameter
                                            .annotations
                                            .iter()
                                            .map(TypeRef::from_annotation)
                                            .collect(),
                                        type_annotations: Vec::new(),
                                        annotation_class_literals: Vec::new(),
                                    })
                                    .collect(),
                            };
                            let erased = parameters
                                .iter()
                                .map(|parameter| {
                                    let ty = ty_of_ref(&parameter.ty, &class_names, &ctp, diags);
                                    semantic_value_parameter_ty(ty, parameter.is_vararg)
                                })
                                .collect::<Vec<_>>();
                            let symbolic = parameters
                                .iter()
                                .map(|parameter| {
                                    let ty = ty_of_ref_silent(
                                        &parameter.ty,
                                        &class_names,
                                        &symbolic_ctp,
                                    );
                                    semantic_value_parameter_ty(ty, parameter.is_vararg)
                                })
                                .collect::<Vec<_>>();
                            let defaults = parameters
                                .iter()
                                .map(|parameter| parameter.has_default)
                                .collect::<Vec<_>>();
                            let required = defaults.iter().filter(|default| !**default).count();
                            let vararg = parameters
                                .iter()
                                .position(|parameter| parameter.is_vararg);
                            let mut call_sig = CallSig::source(
                                parameters
                                    .iter()
                                    .map(|parameter| parameter.name.clone())
                                    .collect(),
                                defaults,
                                symbolic
                                    .iter()
                                    .map(|parameter| match parameter.non_null() {
                                        Ty::Fun(function) => function.params.clone(),
                                        _ => Vec::new(),
                                    })
                                    .collect(),
                                symbolic
                                    .iter()
                                    .map(|parameter| {
                                        matches!(parameter.non_null(), Ty::Fun(function) if function.has_receiver)
                                    })
                                    .collect(),
                                symbolic
                                    .iter()
                                    .map(|parameter| match parameter.non_null() {
                                        Ty::Fun(function) => function.context_count,
                                        _ => 0,
                                    })
                                    .collect(),
                                required,
                                vararg,
                            );
                            call_sig.implicit_integer_coercion = constructor
                                .params
                                .iter()
                                .map(|parameter| {
                                    has_implicit_integer_coercion_annotation(
                                        &parameter.annotations,
                                        &class_names,
                                    )
                                })
                                .collect();
                            (erased, symbolic, call_sig)
                        })
                        .collect::<Vec<_>>();
                    let secondary_ctors = secondary_constructors
                        .iter()
                        .map(|(erased, _, _)| erased.clone())
                        .collect();
                    let secondary_ctor_shapes = secondary_constructors
                        .iter()
                        .map(|(_, symbolic, _)| symbolic.clone())
                        .collect();
                    let secondary_ctor_call_sigs = secondary_constructors
                        .into_iter()
                        .map(|(_, _, call_sig)| call_sig)
                        .collect();
                    let primary_constructor_declaration = compact_headers.and_then(|headers| {
                        streamed_constructor_declaration(headers, compact_classifier?.id, 0)
                    });
                    let secondary_constructor_declarations: Vec<Option<crate::fir::DeclarationId>> =
                        c.secondary_ctors
                            .iter()
                            .enumerate()
                            .map(|(index, _)| {
                                compact_headers.and_then(|headers| {
                                    streamed_constructor_declaration(
                                        headers,
                                        compact_classifier?.id,
                                        u32::try_from(index + 1)
                                            .expect("too many secondary constructors"),
                                    )
                                })
                            })
                            .collect();
                    let primary_constructor_annotations =
                        match (compact_headers, primary_constructor_declaration) {
                            (Some(headers), Some(declaration)) => {
                                streamed_resolved_declaration_annotations(
                                    headers,
                                    declaration,
                                    &table.resolved_annotations,
                                )
                            }
                            _ => c
                                .primary_ctor_annotations
                                .iter()
                                .flatten()
                                .filter_map(|annotation| {
                                    table.resolved_annotation(i as u32, annotation)
                                })
                                .collect(),
                        };
                    let secondary_constructor_annotations = match compact_headers {
                        Some(headers) => secondary_constructor_declarations
                            .iter()
                            .map(|declaration| {
                                compact_declaration_annotation_identities(
                                    headers,
                                    *declaration,
                                    &table.resolved_annotations,
                                )
                            })
                            .collect(),
                        None => c
                            .secondary_ctors
                            .iter()
                            .map(|constructor| {
                                constructor
                                    .annotations
                                    .iter()
                                    .filter_map(|annotation| {
                                        table.resolved_annotation(i as u32, annotation)
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .collect(),
                    };
                    // `inner_of` is the semantic enclosing receiver, not necessarily the lexical name
                    // prefix. They differ for an inner class declared in an enum-entry body:
                    // `E.ENTRY.Inner` is named `E$ENTRY$Inner` but captures an `E` value.
                    let inner_of_ref = c.inner_of.as_deref().and_then(|outer| {
                        projected_enclosing_classifier_identity(
                            file,
                            compact_headers,
                            compact_classifier,
                            outer,
                        )
                    });
                    // A `value class` is represented unboxed as its sole property's type.
                    let value_field = if classifier_is_value {
                        props.first().map(|(n, t, _)| (n.clone(), *t))
                    } else {
                        None
                    };
                    let internal_ref = internal;
                    let explicit_companion = c.companion.and_then(projected_parser_classifier);
                    let companion_internal_ref = explicit_companion.or_else(|| {
                        (!contributed_companion_methods.is_empty())
                            .then(|| type_name_nested_child(internal_ref, "Companion"))
                    });
                    let interfaces_ref: crate::types::TypeNameList = interfaces.into();
                    let callable_signatures = classifier_header
                        .supertypes
                        .iter()
                        .map(|supertype| ty_of_ref_silent(supertype, &class_names, &symbolic_ctp))
                        .filter(|supertype| matches!(supertype, Ty::Fun(_)))
                        .collect::<Vec<_>>();
                    let callable_signature = callable_signatures.first().copied();
                    let super_internal_ref = super_internal
                        .as_ref()
                        .map(|super_internal| type_name(super_internal));
                    table.insert_class_sig(
                        internal_ref,
                        ClassSig {
                            internal: internal_ref,
                            stable_declaration: compact_classifier.map(|stub| stub.id),
                            source_file: i as u32,
                            source_decl: Some(d),
                            visibility: classifier_visibility,
                            annotations: resolved_annotations,
                            applied_annotations: Vec::new(),
                            annotation_class_arguments: resolved_annotation_class_arguments,
                            generated_nested_classifiers,
                            props,
                            declared_props,
                            contextual_props,
                            constants,
                            member_ext_props,
                            member_ext_funs,
                            // The parser represents an implicit primary constructor as `Some` only
                            // when the class declares no secondary constructors. An enum with
                            // explicit secondary constructors has no additional source-level
                            // primary candidate for its entries to select.
                            has_primary_ctor: c.primary_ctor_annotations.is_some(),
                            primary_constructor_declaration,
                            primary_constructor_annotations,
                            ctor_params,
                            ctor_param_shapes,
                            methods,
                            declared_callable_order,
                            source_methods,
                            flags: classifier_flags,
                            inner_of: inner_of_ref,
                            companion_internal: companion_internal_ref,
                            lateinit_props,
                            interfaces: interfaces_ref,
                            callable_signature,
                            callable_signatures,
                            interface_type_args: classifier_header
                                .supertypes
                                .iter()
                                .filter(|interface| interface.name != "<fun>")
                                .filter(|interface| {
                                    parenless_base.as_deref() != Some(interface.name.as_str())
                                })
                                .map(|interface| {
                                    interface
                                        .targs
                                        .iter()
                                        .map(|argument| {
                                            ty_of_ref(
                                                argument,
                                                &header_class_names,
                                                &symbolic_ctp,
                                                diags,
                                            )
                                        })
                                        .collect()
                                })
                                .collect(),
                            delegated_interfaces: classifier_header
                                .delegated_interfaces
                                .iter()
                                .map(|interface| {
                                    ty_of_ref(interface, &header_class_names, &symbolic_ctp, diags)
                                })
                                .collect(),
                            super_internal: super_internal_ref,
                            // An `enum class E`'s implicit superclass is the PARAMETERIZED
                            // `kotlin.Enum<E>`; the declaration writes no argument list, so the
                            // self argument is recorded here rather than read off the source.
                            super_type_args: if classifier_is_enum {
                                vec![Ty::obj_name(internal_ref)]
                            } else {
                                classifier_header
                                    .base
                                    .as_ref()
                                    .or_else(|| {
                                        parenless_base.as_deref().and_then(|base| {
                                            classifier_header
                                                .supertypes
                                                .iter()
                                                .find(|supertype| supertype.name == base)
                                        })
                                    })
                                    .map(|base| base.targs.as_slice())
                                    .unwrap_or_default()
                                    .iter()
                                    .map(|argument| {
                                        ty_of_ref(
                                            argument,
                                            &header_class_names,
                                            &symbolic_ctp,
                                            diags,
                                        )
                                    })
                                    .collect()
                            },
                            super_ctor_params: Vec::new(),
                            ctor_param_names,
                            ctor_implicit_integer_coercion,
                            ctor_vararg: classifier_header
                                .primary_parameters
                                .iter()
                                .position(|parameter| parameter.is_vararg),
                            ctor_defaults,
                            secondary_ctors,
                            secondary_ctor_shapes,
                            secondary_ctor_call_sigs,
                            secondary_constructor_declarations,
                            secondary_constructor_annotations,
                            type_parameters: class_type_parameters,
                            type_parameter_extra_bounds: classifier_header
                                .type_parameters
                                .iter()
                                .map(|source| symbolic_ctp.extra_bounds_of(source))
                                .collect(),
                            captured_type_parameters,
                            metadata_captured_type_parameters,
                            generic_props,
                            nullable_tparam_props,
                            generic_property_shapes,
                            value_field,
                            generic_methods,
                        },
                    );
                    if let Some(companion_internal) =
                        companion_internal_ref.filter(|_| !contributed_companion_methods.is_empty())
                    {
                        if let Some(companion) = table.classes.get_mut(&companion_internal) {
                            for name in contributed_companion_order {
                                let signatures =
                                    contributed_companion_methods.remove(&name).expect(
                                        "a contributed companion name must retain its overloads",
                                    );
                                if !companion.declared_callable_order.contains(&name) {
                                    companion.declared_callable_order.push(name.clone());
                                }
                                companion
                                    .methods
                                    .entry(name)
                                    .or_default()
                                    .extend(signatures);
                            }
                        } else {
                            table.insert_class_sig(
                                companion_internal,
                                ClassSig {
                                    internal: companion_internal,
                                    stable_declaration: None,
                                    source_file: i as u32,
                                    source_decl: None,
                                    visibility: Visibility::Public,
                                    annotations: Vec::new(),
                                    applied_annotations: Vec::new(),
                                    annotation_class_arguments: Vec::new(),
                                    generated_nested_classifiers: Vec::new(),
                                    props: Vec::new(),
                                    declared_props: HashMap::new(),
                                    contextual_props: HashMap::new(),
                                    constants: HashMap::new(),
                                    member_ext_props: HashMap::new(),
                                    member_ext_funs: HashMap::new(),
                                    has_primary_ctor: true,
                                    primary_constructor_declaration: None,
                                    primary_constructor_annotations: Vec::new(),
                                    ctor_params: Vec::new(),
                                    ctor_param_shapes: Vec::new(),
                                    methods: contributed_companion_methods,
                                    declared_callable_order: contributed_companion_order,
                                    source_methods: Vec::new(),
                                    flags: ClassFlags::default().with_final(true).with_object(true),
                                    inner_of: None,
                                    companion_internal: None,
                                    lateinit_props: Default::default(),
                                    interfaces: Default::default(),
                                    interface_type_args: Vec::new(),
                                    delegated_interfaces: Vec::new(),
                                    callable_signature: None,
                                    callable_signatures: Vec::new(),
                                    super_internal: None,
                                    super_type_args: Vec::new(),
                                    super_ctor_params: Vec::new(),
                                    ctor_param_names: Vec::new(),
                                    ctor_implicit_integer_coercion: Vec::new(),
                                    ctor_vararg: None,
                                    ctor_defaults: Vec::new(),
                                    secondary_ctors: Vec::new(),
                                    secondary_ctor_shapes: Vec::new(),
                                    secondary_ctor_call_sigs: Vec::new(),
                                    secondary_constructor_declarations: Vec::new(),
                                    secondary_constructor_annotations: Vec::new(),
                                    type_parameters: crate::types::TypeParameters::default(),
                                    type_parameter_extra_bounds: Vec::new(),
                                    captured_type_parameters: crate::types::TypeParameters::default(
                                    ),
                                    metadata_captured_type_parameters: Vec::new(),
                                    generic_props: HashMap::new(),
                                    nullable_tparam_props: HashMap::new(),
                                    generic_property_shapes: HashMap::new(),
                                    value_field: None,
                                    generic_methods: HashMap::new(),
                                },
                            );
                        }
                    }
                }
                Decl::Property(p) => {
                    let compact_property = compact_declaration;
                    let property_header = match compact_headers.zip(compact_property) {
                        Some((headers, stub)) => streamed_property_header_by_declaration(
                            headers, stub.id,
                        )
                        .expect("a production top-level property must have a compact header"),
                        _ => legacy_property_header(p),
                    };
                    let property_flags = compact_property.map(|stub| stub.flags);
                    let property_name = compact_property
                        .and_then(|stub| stub.lookup_name)
                        .and_then(|name| {
                            compact_headers.and_then(|headers| headers.lookup_names.get(name))
                        })
                        .unwrap_or(&p.name)
                        .to_string();
                    let property_visibility =
                        compact_property.map_or(p.visibility, |stub| stub.visibility);
                    let property_span = compact_property.map_or(p.span, |stub| stub.range);
                    let compact_inference =
                        compact_property.and_then(|stub| stub.signature_inference);
                    let companion_extension = property_flags.map_or_else(
                        || p.is_companion_extension,
                        |flags| flags.has(crate::fir::DeclarationFlags::COMPANION),
                    );
                    let is_const = property_flags.map_or_else(
                        || p.is_const,
                        |flags| flags.has(crate::fir::DeclarationFlags::CONST),
                    );
                    let is_lateinit = property_flags.map_or_else(
                        || p.is_lateinit,
                        |flags| flags.has(crate::fir::DeclarationFlags::LATEINIT),
                    );
                    let is_delegated = property_flags.map_or_else(
                        || p.delegate.is_some(),
                        |flags| flags.has(crate::fir::DeclarationFlags::DELEGATED),
                    );
                    let has_initializer = property_flags.map_or_else(
                        || p.init.is_some(),
                        |flags| flags.has(crate::fir::DeclarationFlags::HAS_INITIALIZER),
                    );
                    let has_custom_getter = property_flags.map_or_else(
                        || p.getter.is_some(),
                        |flags| flags.has(crate::fir::DeclarationFlags::CUSTOM_GETTER),
                    );
                    let has_custom_setter = property_flags.map_or_else(
                        || p.setter.is_some(),
                        |flags| flags.has(crate::fir::DeclarationFlags::CUSTOM_SETTER),
                    );
                    let setter_has_body = property_flags.map_or_else(
                        || {
                            p.setter
                                .as_ref()
                                .is_some_and(|setter| setter.body.is_some())
                        },
                        |flags| flags.has(crate::fir::DeclarationFlags::SETTER_HAS_BODY),
                    );
                    let getter_reads_field = property_flags.map_or_else(
                        || p.getter_reads_field,
                        |flags| flags.has(crate::fir::DeclarationFlags::GETTER_READS_BACKING_FIELD),
                    );
                    let is_abstract = property_flags.map_or_else(
                        || p.is_abstract,
                        |flags| flags.has(crate::fir::DeclarationFlags::ABSTRACT),
                    );
                    let is_external = property_flags.map_or_else(
                        || p.is_external,
                        |flags| flags.has(crate::fir::DeclarationFlags::EXTERNAL),
                    );
                    let is_expect = property_flags.map_or_else(
                        || p.is_expect,
                        |flags| flags.has(crate::fir::DeclarationFlags::EXPECT),
                    );
                    if compact_headers.is_none() {
                        resolve_declaration_type_parameter_annotation_inventory(
                            file,
                            d,
                            &class_names,
                            &mut table.resolved_annotations,
                            &mut annotation_resolution,
                            diags,
                            true,
                        );
                    }
                    if compact_headers.is_some() {
                        validate_top_level_property_shape(
                            property_span,
                            property_header.receiver.is_some(),
                            companion_extension,
                            has_initializer,
                            is_delegated,
                            has_custom_getter,
                            is_lateinit,
                            is_abstract,
                            is_external,
                            is_expect,
                            diags,
                        );
                        validate_context_property_shape(
                            property_span,
                            !property_header.context_parameters.is_empty(),
                            property_header.mutable,
                            has_custom_getter,
                            setter_has_body,
                            has_initializer,
                            is_delegated,
                            is_lateinit,
                            is_const,
                            getter_reads_field,
                            false,
                            diags,
                        );
                    } else {
                        validate_top_level_property(p, diags);
                        validate_context_property(p, false, diags);
                    }
                    // Extension property `val Recv.name: T get() = …`: register by (erased receiver,
                    // name); emitted as a static `getName(Recv)`/`setName(Recv, T)`.
                    if let Some(recv_ref) = &property_header.receiver {
                        // An extension property's own type params (`val <T> Array<T>.length`) scope over
                        // the receiver and declared type — bind them (erased) so the receiver isn't a raw
                        // `Array` and `T` isn't mistaken for an unresolved class.
                        let resolve = |n: &str| class_names.get(n);
                        let erased_tparams = TParams::from_decl_with(
                            &property_header.type_parameters,
                            &property_header.bounds,
                            &resolve,
                        );
                        let semantic_tparams = TParams::symbolic_from_decl_with(
                            &property_header.type_parameters,
                            &property_header.bounds,
                            &resolve,
                        )
                        .alpha_renamed_declaration(
                            &property_header.type_parameters,
                            table.compilation_id,
                            i as u32,
                            property_span.lo,
                        );
                        let erased_receiver = if companion_extension {
                            associated_companion_receiver_ty_from_spelling(
                                recv_ref,
                                property_header.receiver_source_spelling.as_ref(),
                                &class_names,
                                &erased_tparams,
                                diags,
                            )
                        } else {
                            ty_of_ref(recv_ref, &class_names, &erased_tparams, diags)
                        };
                        let recv_ty = if companion_extension {
                            associated_companion_receiver_ty_from_spelling(
                                recv_ref,
                                property_header.receiver_source_spelling.as_ref(),
                                &class_names,
                                &semantic_tparams,
                                diags,
                            )
                        } else {
                            ty_of_ref(recv_ref, &class_names, &semantic_tparams, diags)
                        };
                        let context_params = property_header
                            .context_parameters
                            .iter()
                            .map(|(_, parameter)| {
                                ty_of_ref(parameter, &class_names, &semantic_tparams, diags)
                            })
                            .collect::<Vec<_>>();
                        let mut context_scope = property_header
                            .context_parameters
                            .iter()
                            .zip(&context_params)
                            .filter(|((name, _), _)| name != "_")
                            .map(|((name, _), ty)| (name.clone(), *ty, false))
                            .collect::<Vec<_>>();
                        // An extension getter is inferred in the same receiver scope in which it is
                        // checked. Without `this`, an ordinary expression getter such as
                        // `val String.myLength get() = this.length` disappears from the module's symbol
                        // record entirely, and later reference resolution reports the classifier as an
                        // unresolved value instead of ever seeing the property declaration.
                        if !companion_extension {
                            context_scope.push(("this".to_string(), recv_ty, false));
                        }
                        let receiver_type_parameter = (recv_ref.arg.is_none()
                            && recv_ref.targs.is_empty()
                            && recv_ref.fun_params.is_empty()
                            && !recv_ref.definitely_non_null())
                        .then(|| {
                            property_header
                                .type_parameters
                                .iter()
                                .position(|parameter| parameter == &recv_ref.name)
                        })
                        .flatten();
                        let accepts_nullable_receiver = recv_ref.nullable()
                            || receiver_type_parameter.is_some_and(|index| {
                                declared_tparam_semantic_bound(
                                    &property_header.type_parameters[index],
                                    &property_header.type_parameters,
                                    &property_header.bounds,
                                    &resolve,
                                )
                                .is_none_or(Ty::is_nullable)
                            });
                        let expression_getter = match &p.getter {
                            Some(FunBody::Expr(getter)) => Some((*getter, false)),
                            Some(FunBody::Block(_) | FunBody::None) | None => None,
                        };
                        let inference_body = if compact_headers.is_some() {
                            None
                        } else if companion_extension {
                            p.init
                                .map(|initializer| (initializer, true))
                                .or(expression_getter)
                        } else {
                            expression_getter
                        };
                        let ty = property_header
                            .declared_type
                            .as_ref()
                            .map(|r| ty_of_ref(r, &class_names, &semantic_tparams, diags))
                            .or_else(|| {
                                if compact_inference.is_some() {
                                    return Some(Ty::Pending);
                                }
                                inference_body.map(|(body, _)| {
                                    let source = InferenceSource::file(file, i as u32);
                                    let source = if companion_extension {
                                        source.with_implicit_classifier(recv_ty.obj_internal())
                                    } else {
                                        source.with_implicit_value(Some(recv_ty))
                                    };
                                    infer_lit_ty_scoped(
                                        source,
                                        body,
                                        &class_names,
                                        &context_scope,
                                        &*libraries,
                                        &table,
                                    )
                                })
                            })
                            .unwrap_or(Ty::Error);
                        // An EXTENSION property the walk could not type is not an error yet, exactly
                        // as a plain one is not: its getter may read a declaration in a file the walk
                        // has not reached, and it is typed in a receiver scope the walk cannot build
                        // (`val <T : CharSequence> T.z get() = { x: T -> this }`). Record the marker
                        // and queue the declaration; the engine runs the real checker over it, and
                        // `settle` turns a decline back into the error placeholder this used to
                        // publish immediately.
                        let ty = match (
                            ty.mentions_error(),
                            property_header.declared_type.as_ref(),
                            inference_body,
                        ) {
                            (true, None, Some((expression, is_eager))) => {
                                deferred_properties.push(DeferredProperty {
                                    key: DeclKey::declaration(i as u32, d.0),
                                    file_index: i as u32,
                                    name: property_name.clone(),
                                    span: p.span,
                                    delegate_of: None,
                                    backing_field: false,
                                    expression: Some(expression),
                                    scope: context_scope.clone(),
                                    kind: DeferredKind::TopLevel {
                                        position: p.span.lo,
                                        is_eager,
                                        has_context_params: !context_params.is_empty(),
                                        has_receiver: true,
                                    },
                                });
                                Ty::Pending
                            }
                            _ => ty,
                        };
                        crate::trace_compiler!(
                            "signature_inference",
                            "extension property name={} receiver={recv_ty:?} getter={:?} inferred={ty:?}",
                            property_name, compact_inference,
                        );
                        // A delegated extension's result is owned by the compact signature graph.
                        // Register its declaration shape even before that graph has solved the
                        // result: signature evaluation needs the receiver/name family in order to
                        // turn `O.prop` into a stable declaration dependency. The pending-free
                        // result is projected back by stable identity before Pass 2 starts.
                        if erased_receiver != Ty::Error
                            && recv_ty != Ty::Error
                            && (ty != Ty::Error || is_delegated)
                        {
                            let receiver_key = erased_receiver.erased_recv();
                            // Preserve the written receiver shape. A bare type parameter already
                            // admits nullable instantiations when its bound permits them; wrapping
                            // it in `?` changes inference (`String?` against `T?` binds `T = String`)
                            // and loses the use-site nullable type argument carried by `T` itself.
                            let declared_receiver = if recv_ref.nullable() {
                                Ty::nullable(recv_ty)
                            } else {
                                recv_ty
                            };
                            let key = (receiver_key, property_name.clone());
                            // Equal erased signatures conflict only when they share a facade.
                            let package = source_packages[i].replace('.', "/");
                            let signature = ExtPropSig {
                                formal_names: property_header.type_parameters.clone(),
                                formals: property_header
                                    .type_parameters
                                    .iter()
                                    .map(|source| {
                                        semantic_tparams
                                            .bound(source)
                                            .ty_param_name()
                                            .unwrap_or(source)
                                            .to_owned()
                                    })
                                    .collect(),
                                formal_bounds: property_header
                                    .type_parameters
                                    .iter()
                                    .map(|parameter| {
                                        semantic_tparams
                                            .bound(parameter)
                                            .ty_param_bound()
                                            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                                    })
                                    .collect(),
                                receiver: declared_receiver,
                                ty,
                                is_var: property_header.mutable,
                                is_companion_extension: companion_extension,
                                getter_name: property_getter_name(&property_name),
                                setter_name: property_header
                                    .mutable
                                    .then(|| property_setter_name(&property_name)),
                                context_params,
                                accepts_nullable_receiver,
                                source: (i as u32, d.0),
                                package,
                                visibility: property_visibility,
                                annotations: declaration_annotation_identities(
                                    compact_headers,
                                    compact_property.map(|stub| stub.id),
                                    &table.resolved_annotations,
                                    || resolved_annotation_identities(&p.annotations, &class_names),
                                ),
                                stable_declaration: compact_property.map(|stub| stub.id),
                            };
                            let overloads = table.ext_props.entry(key).or_default();
                            if overloads.iter().any(|existing| {
                                existing.package == signature.package
                                    && extension_property_headers_alpha_equivalent(
                                        existing, &signature,
                                    )
                                    && !(existing.visibility.is_private()
                                        && signature.visibility.is_private()
                                        && existing.source.0 != i as u32)
                            }) {
                                diags.error(property_span, format!("krusty: conflicting extension property '{}' (same erased receiver)", property_name));
                            } else {
                                overloads.push(signature);
                            }
                        }
                        continue;
                    }
                    // A top-level *computed* property (custom getter, no initializer) is emitted as
                    // `getX()`. An expression getter may supply its inferred type; block getters still
                    // require an annotation because the lightweight signature inferer has no block flow.
                    let is_computed = has_custom_getter && !has_initializer;
                    let resolve = |name: &str| class_names.get(name);
                    let property_tparams = TParams::symbolic_from_decl_with(
                        &property_header.type_parameters,
                        &property_header.bounds,
                        &resolve,
                    )
                    .alpha_renamed_declaration(
                        &property_header.type_parameters,
                        table.compilation_id,
                        i as u32,
                        property_span.lo,
                    );
                    let context_params = property_header
                        .context_parameters
                        .iter()
                        .map(|(_, parameter)| {
                            ty_of_ref(parameter, &class_names, &property_tparams, diags)
                        })
                        .collect::<Vec<_>>();
                    let context_scope = property_header
                        .context_parameters
                        .iter()
                        .zip(&context_params)
                        .filter(|((name, _), _)| name != "_")
                        .map(|((name, _), ty)| (name.clone(), *ty, false))
                        .collect::<Vec<_>>();
                    // A top-level backing-field property with a CUSTOM accessor (`val x = init get() =
                    // field`, `var y = init set(v){…}`) is lowered as a facade static + custom
                    // `getX`/`setX` (with `field` bound to the static). Reject only a custom accessor with
                    // NO backing-field initializer — there is nothing for `field` to bind to.
                    let has_custom_accessor = has_custom_getter || has_custom_setter;
                    if has_custom_accessor && !has_initializer && !is_computed {
                        diags.error(
                            property_span,
                            "krusty: top-level property custom accessors are not supported"
                                .to_string(),
                        );
                    }
                    // A delegated property `val x: T by Del()`: type is the annotation if present, else the
                    // delegate's `getValue` return type. Resolving the read-type here lets `val a = x`
                    // infer. Checked FIR carries the selected delegate operations into common
                    // lowering; an unresolvable delegate type yields `Error` and the file skips.
                    let inferred_ty = if compact_headers.is_some() {
                        match property_header.declared_type.as_ref() {
                            Some(declared) => {
                                ty_of_ref(declared, &class_names, &property_tparams, diags)
                            }
                            None if compact_inference.is_some() => Ty::Pending,
                            None => Ty::Error,
                        }
                    } else if let Some(delegate) = p.delegate {
                        match property_header.declared_type.as_ref() {
                            Some(declared) => {
                                ty_of_ref(declared, &class_names, &property_tparams, diags)
                            }
                            None => {
                                // The legacy publisher cannot type a delegate in its declaration
                                // scope. Production never enters this queue: the compact signature
                                // graph owns the delegate expression and publishes its final type.
                                deferred_properties.push(DeferredProperty {
                                    key: DeclKey::declaration(i as u32, d.0),
                                    file_index: i as u32,
                                    name: property_name.clone(),
                                    span: property_span,
                                    delegate_of: Some(Ty::Null),
                                    backing_field: false,
                                    expression: Some(delegate),
                                    scope: Vec::new(),
                                    kind: DeferredKind::TopLevel {
                                        position: property_span.lo,
                                        is_eager: true,
                                        has_context_params: false,
                                        has_receiver: false,
                                    },
                                });
                                Ty::Pending
                            }
                        }
                    } else {
                        // The legacy path defers every implicit property result. Production uses
                        // `compact_inference` above and never inspects getter/initializer syntax.
                        match (property_header.declared_type.as_ref(), &p.getter) {
                            (Some(declared), _) => {
                                ty_of_ref(declared, &class_names, &property_tparams, diags)
                            }
                            (None, Some(FunBody::Expr(_))) if is_computed => Ty::Pending,
                            (None, _) => Ty::Pending,
                        }
                    };
                    // `null` is the expression type used by applicability, while an inferred
                    // declaration `val x = null` has Kotlin type `Nothing?`. Declaration symbols
                    // must never publish the literal-only `Null` sentinel.
                    let ty = inferred_declaration_ty(inferred_ty);
                    // A property this walk could not type is not an error yet. The declaration it
                    // reads may live in a file the walk has not reached — signature collection
                    // visits files in ARGUMENT order — so record it and let the engine resolve it on
                    // demand once the walk is over. Diagnosing here is what made the answer depend
                    // on the order the compiler was asked in.
                    if compact_headers.is_none()
                        && ty == Ty::Pending
                        && property_header.declared_type.is_none()
                        && p.delegate.is_none()
                        && (p.init.is_some() || p.getter.is_some())
                    {
                        deferred_properties.push(DeferredProperty {
                            key: DeclKey::declaration(i as u32, d.0),
                            file_index: i as u32,
                            name: property_name.clone(),
                            span: property_span,
                            delegate_of: None,
                            backing_field: false,
                            expression: p.init.or(match &p.getter {
                                Some(FunBody::Expr(getter)) if is_computed => Some(*getter),
                                // A block getter has no expression the signature pass can read; it
                                // declines, which is the diagnostic asking for an annotation.
                                _ => None,
                            }),
                            scope: context_scope.clone(),
                            kind: DeferredKind::TopLevel {
                                position: property_span.lo,
                                is_eager: p.init.is_some(),
                                has_context_params: !context_params.is_empty(),
                                has_receiver: false,
                            },
                        });
                    }
                    let storage_ty = if compact_headers.is_some() {
                        property_header
                            .backing_field_type
                            .as_ref()
                            .map(|field| ty_of_ref(field, &class_names, &property_tparams, diags))
                            .or_else(|| {
                                (compact_inference
                                    == Some(
                                        crate::fir::InferredSignatureKind::BackingFieldInitializer,
                                    ))
                                .then_some(Ty::Pending)
                            })
                    } else {
                        p.explicit_backing_field.as_ref().map(|field| {
                            field
                                .ty
                                .as_ref()
                                .map(|field_ty| {
                                    ty_of_ref(field_ty, &class_names, &property_tparams, diags)
                                })
                                .or_else(|| {
                                    p.init.map(|initializer| {
                                        deferred_properties.push(DeferredProperty {
                                            key: DeclKey::declaration(i as u32, d.0),
                                            file_index: i as u32,
                                            name: property_name.clone(),
                                            span: property_span,
                                            delegate_of: None,
                                            backing_field: true,
                                            expression: Some(initializer),
                                            scope: context_scope.clone(),
                                            kind: DeferredKind::TopLevel {
                                                position: property_span.lo,
                                                is_eager: true,
                                                has_context_params: false,
                                                has_receiver: false,
                                            },
                                        });
                                        Ty::Pending
                                    })
                                })
                                .unwrap_or(Ty::Error)
                        })
                    };
                    table.source_props.insert(
                        (i as u32, d.0),
                        SourcePropertySig {
                            name: property_name.clone(),
                            formals: property_header
                                .type_parameters
                                .iter()
                                .filter_map(|source| property_tparams.bound(source).ty_param_name())
                                .map(str::to_string)
                                .collect(),
                            formal_bounds: property_header
                                .type_parameters
                                .iter()
                                .map(|source| {
                                    property_tparams
                                        .bound(source)
                                        .ty_param_bound()
                                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                                })
                                .collect(),
                            ty,
                            storage_ty,
                            is_var: property_header.mutable,
                            is_const,
                            is_lateinit,
                            compile_time_constant: if compact_headers.is_some()
                                || property_header.mutable
                            {
                                None
                            } else {
                                p.init
                                    .and_then(|init| source_literal_constant(file, init, ty))
                            },
                            implicit_integer_coercion: if compact_headers.is_some() {
                                header_type_has_annotation(
                                    &property_header.annotations,
                                    &class_names,
                                    type_name("kotlin/internal/ImplicitIntegerCoercion"),
                                )
                            } else {
                                has_implicit_integer_coercion_annotation(
                                    &p.annotations,
                                    &class_names,
                                )
                            },
                            context_params: context_params.clone(),
                            context_param_names: property_header
                                .context_parameters
                                .iter()
                                .map(|(name, _)| name.clone())
                                .collect(),
                            context_parameter_identities: p
                                .context_params
                                .iter()
                                .enumerate()
                                .map(|(ordinal, parameter)| match parameter.context_kind {
                                    crate::types::ContextParameterKind::Named => {
                                        crate::fir::ResolvedParameterIdentity::ContextValue {
                                            ordinal: ordinal as u32,
                                            source_name: parameter.name.as_str().into(),
                                        }
                                    }
                                    crate::types::ContextParameterKind::Anonymous => {
                                        crate::fir::ResolvedParameterIdentity::AnonymousContextParameter {
                                            ordinal: ordinal as u32,
                                        }
                                    }
                                    crate::types::ContextParameterKind::LegacyReceiver => {
                                        crate::fir::ResolvedParameterIdentity::LegacyContextReceiver {
                                            ordinal: ordinal as u32,
                                        }
                                    }
                                    crate::types::ContextParameterKind::None => unreachable!(
                                        "a property context prefix must carry a context role"
                                    ),
                                })
                                .collect(),
                            package: source_packages[i].replace('.', "/"),
                            visibility: property_visibility,
                            setter_visibility: property_header.setter_visibility,
                            setter_parameter_name: property_header.setter_parameter_name.clone(),
                            read_stability: if property_header.mutable
                                || has_custom_getter
                                || is_delegated
                                || is_external
                                || is_expect
                                || !context_params.is_empty()
                            {
                                crate::libraries::PropertyReadStability::Unstable
                            } else {
                                crate::libraries::PropertyReadStability::Stable
                            },
                            annotations: declaration_annotation_identities(
                                compact_headers,
                                compact_property.map(|stub| stub.id),
                                &table.resolved_annotations,
                                || resolved_annotation_identities(&p.annotations, &class_names),
                            ),
                            stable_declaration: compact_property.map(|stub| stub.id),
                        },
                    );
                    if context_params.is_empty() {
                        crate::trace_compiler!(
                            "signature_inference",
                            "publish top-level property name={} type={ty:?} source=({}, {})",
                            property_name,
                            i,
                            d.0,
                        );
                        table.props.insert(
                            property_name.clone(),
                            (ty, property_header.mutable, is_const),
                        );
                    } else {
                        table.context_prop_names.insert(property_name);
                    }
                }
            }
        }
    }

    // The engine types a deferred declaration by running the REAL checker over its body, and the
    // checker resolves classifiers and callables through the table's platform. Install it before
    // that runs: without it every classpath and built-in name — `Int` included — is unresolved, and
    // every deferred declaration declines.
    // Source type ALIASES are name-resolution facts the deferred run needs: it resolves a
    // declaration by running the real checker, and `val p = AliasedCell(MyClass())` is an
    // unresolved reference until the alias's expansion is installed. Registered here rather
    // than after resolution for the same reason `libraries` is — a table the checker reads
    // must be complete before the checker runs.

    // Retain source aliases as per-file name-resolution edges. A copied `ClassSig` under a simple
    // alias key would violate the class table's internal-name invariant and make hierarchy walks,
    // module providers, and direct lookups disagree about the same table.
    for file_index in 0..source_packages.len() {
        for (alias, _, _) in &file_type_aliases[file_index] {
            if let Some(internal) = file_class_names[file_index]
                .get_class(alias)
                .filter(|internal| table.classes.contains_key(internal))
            {
                table
                    .source_class_aliases
                    .insert((file_index as u32, alias.clone()), internal);
            }
        }
        // Resolve every alias declaration to the one authoritative expansion consumed by metadata.
        // Alias chains are structurally expanded first; the remaining classifier spellings then bind
        // through this declaration file's ordinary imports and class table.
        let package_source = &source_packages[file_index];
        let package = package_source.replace('.', "/");
        let mut visible_aliases = Vec::new();
        // Qualified spellings are absolute and independent of import precedence.
        for (declaration_file_index, declaration_package) in source_packages.iter().enumerate() {
            for (name, formals, target) in &file_type_aliases[declaration_file_index] {
                let qualified = if declaration_package.is_empty() {
                    name.clone()
                } else {
                    format!("{declaration_package}.{name}")
                };
                visible_aliases.push((qualified, formals.clone(), target.clone()));
            }
        }
        // Star imports contribute below same-package declarations.
        for imported_package in source_imports[file_index]
            .iter()
            .filter(|import| import.wildcard)
            .map(|import| import.path.as_str())
        {
            for (declaration_file_index, _) in source_packages
                .iter()
                .enumerate()
                .filter(|candidate| candidate.1.as_str() == imported_package)
            {
                visible_aliases.extend(file_type_aliases[declaration_file_index].iter().cloned());
            }
        }
        // Same-package declarations outrank star imports and include aliases from sibling files.
        for (declaration_file_index, _) in source_packages
            .iter()
            .enumerate()
            .filter(|candidate| candidate.1 == package_source)
        {
            visible_aliases.extend(file_type_aliases[declaration_file_index].iter().cloned());
        }
        // Explicit imports are final and may rename the lookup spelling.
        for import in source_imports[file_index]
            .iter()
            .filter(|import| !import.wildcard)
        {
            let spelling = &import.visible_name;
            let internal_path = import.path.replace('.', "/");
            for (declaration_file_index, declaration_package) in source_packages.iter().enumerate()
            {
                let declaration_package = declaration_package.replace('.', "/");
                for (name, formals, target) in &file_type_aliases[declaration_file_index] {
                    let qualified = if declaration_package.is_empty() {
                        name.clone()
                    } else {
                        format!("{declaration_package}/{name}")
                    };
                    if qualified == internal_path {
                        visible_aliases.push((spelling.clone(), formals.clone(), target.clone()));
                    }
                }
            }
        }
        for (alias, formals, target) in &file_type_aliases[file_index] {
            let names = &file_class_names[file_index];
            let Some((expansion, expansion_spelling)) = resolve_source_alias_expansion(
                target,
                formals,
                &visible_aliases,
                names,
                &table.alias_expansion_spellings,
                diags,
            ) else {
                continue;
            };
            let fqn = if package.is_empty() {
                alias.clone()
            } else {
                format!("{package}/{alias}")
            };
            let identity = type_name(&fqn);
            if let Some(target) = expansion.kotlin_class_internal() {
                table.source_alias_fqns.insert(identity, target);
            }
            table
                .source_alias_expansions
                .insert(identity, (formals.clone(), expansion));
            // Recorded unconditionally: even a right-hand side that spells no alias carries the
            // formals and expansion a use site needs to place ITS spellings into the parameter
            // positions (`typealias Boxed<T> = PBox<T, T>` spells nothing, yet `Boxed<Cargo>`
            // abbreviates both expanded arguments).
            table
                .alias_expansion_spellings
                .insert(identity, (expansion_spelling, formals.clone(), expansion));
        }
        // Nested aliases inhabit their declaring classifier's lexical namespace. Their semantic
        // identity follows the slash-separated path recorded by an explicit import; the expansion,
        // not that namespace coordinate, supplies the classifier used by construction and type use.
        if let Some(headers) = compact_headers {
            publish_compact_nested_aliases(
                &mut table,
                headers,
                compact_local_contexts,
                &file_class_names,
                crate::fir::SourceFileId::from_raw(file_index as u32),
                &visible_aliases,
                diags,
            );
            continue;
        }
        let file = &files[file_index];
        for (class_declaration, class) in file
            .decls
            .iter()
            .filter_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) => Some((declaration, class)),
                Decl::Fun(_) | Decl::Property(_) => None,
            })
            .filter(|(_, class)| !class.type_aliases.is_empty())
        {
            let mut nested_visible = visible_aliases.clone();
            nested_visible.extend(class.type_aliases.iter().map(|alias| {
                (
                    alias.name.clone(),
                    alias.type_params.clone(),
                    alias.target.clone(),
                )
            }));
            let mut nested_names = file_class_names[file_index].clone();
            let alias_owner = type_name(&class_internal(file, &class.name));
            extend_lexical_nested_classifier_names(
                file,
                &HashMap::from([(alias_owner, 0)]),
                &mut nested_names,
            );
            if file.is_local_declaration(class_declaration) {
                // Hoisting gives a local classifier a unique runtime name (`fun.Local`) but must
                // not erase the source binding (`Local`) used by a nested alias target. Only sibling
                // local classifiers owned by the same bounded declaration participate here; a
                // same-spelled class in another function is not a competing lexical candidate.
                let lexical_owner = class.name.rsplit_once('.').map(|(owner, _)| owner);
                for &declaration in file.local_class_decls.values() {
                    let Decl::Class(candidate) = file.decl(declaration) else {
                        continue;
                    };
                    if candidate.name.rsplit_once('.').map(|(owner, _)| owner) != lexical_owner {
                        continue;
                    }
                    let source_name = candidate
                        .name
                        .rsplit('.')
                        .next()
                        .expect("a local classifier has a source-name segment");
                    let internal = type_name(&class_internal(file, &candidate.name));
                    match nested_names.get_class(source_name) {
                        Some(previous) if previous != internal => {
                            nested_names.mark_ambiguous(source_name.to_owned());
                        }
                        Some(_) => {}
                        None => {
                            nested_names.insert_name(source_name.to_owned(), internal);
                        }
                    }
                }
            }
            for alias in &class.type_aliases {
                let Some((expansion, expansion_spelling)) = resolve_source_alias_expansion(
                    &alias.target,
                    &alias.type_params,
                    &nested_visible,
                    &nested_names,
                    &table.alias_expansion_spellings,
                    diags,
                ) else {
                    continue;
                };
                let owner = class.name.replace('.', "/");
                let identity = type_name(&if package.is_empty() {
                    format!("{owner}/{}", alias.name)
                } else {
                    format!("{package}/{owner}/{}", alias.name)
                });
                if let Some(target) = expansion.kotlin_class_internal() {
                    table.source_alias_fqns.insert(identity, target);
                }
                table
                    .source_alias_expansions
                    .insert(identity, (alias.type_params.clone(), expansion));
                table.alias_expansion_spellings.insert(
                    identity,
                    (expansion_spelling, alias.type_params.clone(), expansion),
                );
            }
        }
    }

    // Class/body inventories bind additional annotation occurrences after the initial detached-ref
    // pass. Apply the common-source gate again at the completed declaration boundary, then normalize
    // policies for any library annotation first encountered in those later inventories.
    table
        .resolved_annotations
        .retain(|(file, _, _), annotation| {
            compact_headers.map_or_else(
                || {
                    files
                        .get(*file as usize)
                        .is_some_and(|source| source.is_common)
                },
                |headers| {
                    headers
                        .scopes
                        .file(crate::fir::SourceFileId::from_raw(*file))
                        .is_some_and(|source| source.is_common)
                },
            ) || !libraries.is_optional_expectation(*annotation)
        });
    normalize_referenced_library_annotations(&mut table, &*libraries);
    table.libraries = libraries;
    // A deferred declaration may read a MEMBER function whose return is itself inferred
    // (`class W<T>(val value: T) { fun get() = value }` / `val x = W("OK").get()`). Member returns
    // are pre-inferred after collection returns, so during this run that return was undetermined and
    // the read erased to the parameter's bound — `x` published `Any`. Pre-inferring first gives the
    // run something to read; the pass after collection still runs and still refines, because this is
    // a fixpoint rather than a single sweep.
    if compact_headers.is_none() {
        resolve_deferred_properties(files, &mut table, &deferred_properties, true, diags);
    } else {
        // Every non-local inferred property in production has a compact graph node. The legacy
        // deferred fixpoint reads initializer/getter AST and therefore both duplicates semantics
        // and prevents bounded-unit release. Keep only the provisional declaration shapes here;
        // stable graph projection replaces them before Pass 2.
        drop(deferred_properties);
    }
    // The streamed path still needs provisional `Pending` declarations as demandable candidates
    // while the compact graph is solving. Graph finalization consumes those states and stable-ID
    // projection replaces them before Pass 2. The non-streamed API has no later solver, so it must
    // retire every undetermined state here.
    if compact_headers.is_none() {
        fail_undetermined_returns(&mut table);
        assert!(
            !module_signatures_mention_pending(&table),
            "signature finalization must retire every pending semantic type before body checking",
        );
    }

    if compact_headers.is_none() {
        commit_top_level_conflict_groups(
            &mut table,
            &top_level_fun_groups,
            &pending_conflict_diagnostics,
            reserved_conflict_diagnostic_bytes,
            diags,
        );
        for (name, signatures) in &table.funs {
            for signature in signatures {
                let Some((file, declaration)) = signature.source_file.zip(signature.source_decl)
                else {
                    continue;
                };
                let Some(key) =
                    TopLevelFunctionConflictKey::from_signature(signature, name.clone())
                else {
                    continue;
                };
                let source_is_local = signature.visibility.is_private()
                    || files
                        .get(file as usize)
                        .and_then(|source| match source.decl(declaration) {
                            Decl::Fun(function) => Some(is_kotlin_main_entry_point(
                                function,
                                &signature.params,
                                signature.ret,
                            )),
                            _ => None,
                        })
                        .unwrap_or(false);
                let retained_for_recovery = table
                    .conflicting_top_level_candidates
                    .get(&key)
                    .is_some_and(|candidates| {
                        !source_is_local || candidates.by_file.contains_key(&file)
                    });
                if retained_for_recovery {
                    table
                        .conflicting_top_level_key_by_source
                        .insert((file, declaration.0), key);
                }
            }
        }
        for (name, receivers) in &table.ext_funs {
            for signatures in receivers.values() {
                for signature in signatures {
                    let Some((file, declaration, _receiver)) = signature
                        .source_file
                        .zip(signature.source_decl)
                        .zip(signature.source_receiver)
                        .map(|((file, declaration), receiver)| (file, declaration, receiver))
                    else {
                        continue;
                    };
                    let Some(key) =
                        TopLevelFunctionConflictKey::from_signature(signature, name.clone())
                    else {
                        continue;
                    };
                    let retained_for_recovery = table
                        .conflicting_top_level_candidates
                        .get(&key)
                        .is_some_and(|candidates| {
                            !signature.visibility.is_private()
                                || candidates.by_file.contains_key(&file)
                        });
                    if retained_for_recovery {
                        table
                            .conflicting_top_level_key_by_source
                            .insert((file, declaration.0), key);
                    }
                }
            }
        }
    }

    if let Some(headers) = compact_headers {
        collect_compact_declared_spellings(&mut table, headers, &file_class_names);
    } else {
        collect_declared_spellings(&mut table, files, &file_class_names);
    }
    collect_stable_visibility_suppressions(&mut table, compact_headers);

    table.class_names = class_names;
    if let Some(headers) = compact_headers {
        break_supertype_cycles(&mut table, diags, |internal, signature, components| {
            compact_cycle_span(internal, signature, headers, components)
        });
    } else {
        break_supertype_cycles(&mut table, diags, |internal, signature, components| {
            legacy_cycle_span(internal, signature, files, components)
        });
    }
    table.finish_module_mutation();
    table
}
