//! Binding of annotation references written on declarations.
//!
//! An annotation is resolved once, through the ordinary scope and import rules, and recorded
//! against the occurrence's source span. Signature projection then reads the bound identity by
//! stable declaration; no later phase recovers an annotation from its spelling.

use super::*;

pub(in crate::resolve) fn resolved_annotation_identities(
    annotations: &[AnnotationRef],
    class_names: &ClassNames,
) -> Vec<TypeName> {
    annotations
        .iter()
        .filter_map(|annotation| class_names.get_class(&annotation.name))
        .collect()
}

pub(in crate::resolve) fn resolved_header_annotation_identities(
    annotations: &[TypeRef],
    class_names: &ClassNames,
) -> Vec<TypeName> {
    annotations
        .iter()
        .filter_map(|annotation| class_names.classifier_binding(&annotation.name).ok())
        .collect()
}

/// One compact declaration's annotation identities, consumed from the bindings annotation
/// resolution already selected. Signature projection never re-enters classifier lookup for a
/// declaration that has a stable identity.
pub(in crate::resolve) fn compact_declaration_annotation_identities(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: Option<crate::fir::DeclarationId>,
    bindings: &HashMap<(u32, u32, u32), TypeName>,
) -> Vec<TypeName> {
    streamed_resolved_declaration_annotations(
        headers,
        declaration.expect("a compact source declaration has a stable declaration"),
        bindings,
    )
}

/// The same identities for a declaration that may also arrive through the non-streamed path used
/// for dependency modules, which still reads the source spelling. `source` is evaluated only on
/// that path, so the compact path pays no source lookup.
pub(in crate::resolve) fn declaration_annotation_identities(
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
    declaration: Option<crate::fir::DeclarationId>,
    bindings: &HashMap<(u32, u32, u32), TypeName>,
    source: impl FnOnce() -> Vec<TypeName>,
) -> Vec<TypeName> {
    match compact_headers {
        Some(headers) => compact_declaration_annotation_identities(headers, declaration, bindings),
        None => source(),
    }
}

/// Bind declaration type-parameter annotations in the exact classifier scope already built for
/// their owning signature. Source spelling is lookup input once; Pass 1 temporarily associates the
/// result with the occurrence, then projects declaration-owned facts to stable identities.
pub(in crate::resolve) struct DeclarationAnnotationResolution<'a> {
    pub(in crate::resolve) file_index: u32,
    pub(in crate::resolve) attempted: &'a mut std::collections::HashSet<(u32, u32, u32)>,
}

pub(in crate::resolve) fn resolve_declaration_type_parameter_annotations(
    file: &File,
    declaration_start: u32,
    class_names: &ClassNames,
    resolved: &mut HashMap<(u32, u32, u32), TypeName>,
    resolution: &mut DeclarationAnnotationResolution<'_>,
    diags: &mut DiagSink,
) {
    let Some(parameters) = file
        .declaration_type_parameter_annotations
        .get(&declaration_start)
    else {
        return;
    };
    for annotation in parameters
        .iter()
        .flat_map(|parameter| &parameter.annotations)
    {
        let key = (
            resolution.file_index,
            annotation.span.lo,
            annotation.span.hi,
        );
        if !resolution.attempted.insert(key) {
            continue;
        }
        let Some(identity) = class_names.get_class(&annotation.name) else {
            diags.error(
                annotation.span,
                format!("unresolved reference '{}'.", annotation.name),
            );
            continue;
        };
        let previous = resolved.insert(key, identity);
        debug_assert!(
            previous.is_none_or(|previous| previous == identity),
            "annotation occurrence {key:?} was rebound from {previous:?} to {identity:?}"
        );
    }
}

/// Add direct nested classifiers from the supplied lexical owners. Owners carry lexical-precedence
/// ranks (nearest is lowest); one arena scan therefore handles ordinary nested classes, inherited
/// lexical owners, and enum-entry anonymous subclass scopes without origin-specific name probing.
pub(in crate::resolve) fn extend_lexical_nested_classifier_names(
    file: &File,
    lexical_owner_ranks: &HashMap<TypeName, usize>,
    class_names: &mut ClassNames,
) {
    let mut lexical_nested = HashMap::<String, (usize, TypeName)>::new();
    for &declaration in &file.decls {
        let Decl::Class(nested) = file.decl(declaration) else {
            continue;
        };
        let Some((owner, simple)) = nested.name.rsplit_once('.') else {
            continue;
        };
        let owner = type_name(&class_internal(file, owner));
        let Some(&rank) = lexical_owner_ranks.get(&owner) else {
            continue;
        };
        let internal = class_names
            .get(&nested.name)
            .unwrap_or_else(|| type_name(&class_internal(file, &nested.name)));
        match lexical_nested.entry(simple.to_string()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert((rank, internal));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) if rank < entry.get().0 => {
                entry.insert((rank, internal));
            }
            std::collections::hash_map::Entry::Occupied(_) => {}
        }
    }
    for (simple, (_, internal)) in lexical_nested {
        class_names.insert_name(simple, internal);
    }
}

/// Bind annotation occurrences written in one class body's lexical classifier scope. Annotation
/// references are also retained in `detached_type_refs`, but rebinding a member annotation later
/// from file scope loses own/inherited nested classifiers. The smallest enclosing class owns the
/// occurrence; nested declarations will bind their own bodies when their header is visited.
pub(in crate::resolve) fn resolve_class_body_annotation_references(
    file: &File,
    declaration: DeclId,
    class: &ClassDecl,
    class_names: &ClassNames,
    file_index: u32,
    resolved: &mut HashMap<(u32, u32, u32), TypeName>,
) {
    let declaration_type_parameter_annotations = file
        .declaration_type_parameter_annotations
        .values()
        .flatten()
        .flat_map(|parameter| {
            parameter
                .annotations
                .iter()
                .map(|annotation| annotation.span)
        })
        .collect::<std::collections::HashSet<_>>();
    for reference in &file.detached_type_refs {
        if reference.span.lo < class.span.lo || reference.span.hi > class.span.hi {
            continue;
        }
        // These occurrences belong to the declaration inventory below, which has the exact
        // function/local/enum-entry lexical scope. The broad class-body walk must never pre-bind
        // them through an enclosing classifier rung.
        if declaration_type_parameter_annotations.contains(&reference.span) {
            continue;
        }
        let belongs_to_nested = file.decls.iter().copied().any(|candidate| {
            if candidate == declaration {
                return false;
            }
            let Decl::Class(candidate) = file.decl(candidate) else {
                return false;
            };
            candidate.span.lo >= class.span.lo
                && candidate.span.hi <= class.span.hi
                && reference.span.lo >= candidate.span.lo
                && reference.span.hi <= candidate.span.hi
        });
        if belongs_to_nested {
            continue;
        }
        if let Some(identity) = class_names.get_class(&reference.name) {
            resolved.insert((file_index, reference.span.lo, reference.span.hi), identity);
        }
    }
}

fn enum_entry_expression_roots(
    entry: &crate::ast::AstEnumEntry,
) -> impl Iterator<Item = ExprId> + '_ {
    entry
        .annotation_args
        .iter()
        .flatten()
        .copied()
        .chain(entry.args.iter().copied())
        .chain(entry.methods.iter().flat_map(fun_expression_roots))
        .chain(entry.props.iter().flat_map(property_expression_roots))
        .chain(entry.init_order.iter().filter_map(|step| match step {
            ClassInit::Block(body) => Some(*body),
            ClassInit::PropInit(_) => None,
        }))
}

/// Resolve every local function reachable below the supplied expression roots. Local classes are
/// deliberately skipped here: parser normalization hoists them into `File::decls`, where the class
/// inventory runs with that declaration's own lexical classifier scope. The iterative walk keeps this
/// ownership pass bounded by arena size rather than expression nesting depth.
pub(in crate::resolve) fn resolve_local_type_parameter_annotation_inventory(
    file: &File,
    roots: impl IntoIterator<Item = ExprId>,
    class_names: &ClassNames,
    resolved: &mut HashMap<(u32, u32, u32), TypeName>,
    resolution: &mut DeclarationAnnotationResolution<'_>,
    diags: &mut DiagSink,
) {
    let mut expressions = roots.into_iter().collect::<Vec<_>>();
    let mut seen_expressions = std::collections::HashSet::new();
    let mut seen_statements = std::collections::HashSet::new();
    while let Some(expression) = expressions.pop() {
        if !seen_expressions.insert(expression) {
            continue;
        }
        let mut children = Vec::new();
        let mut statements = Vec::new();
        file.any_child_expr(
            expression,
            &mut |child| {
                children.push(child);
                false
            },
            &mut |statement| {
                statements.push(statement);
                false
            },
        );
        expressions.extend(children);
        for statement in statements {
            if !seen_statements.insert(statement) {
                continue;
            }
            match file.stmt(statement) {
                Stmt::LocalFun(function) => {
                    resolve_declaration_type_parameter_annotations(
                        file,
                        function.signature_span.lo,
                        class_names,
                        resolved,
                        resolution,
                        diags,
                    );
                    file.any_fun_expr(function, &mut |root| {
                        expressions.push(root);
                        false
                    });
                }
                // Hoisted declarations receive their own exact class inventory.
                Stmt::LocalClass(_) => {}
                _ => {
                    file.any_child_stmt(statement, &mut |child| {
                        expressions.push(child);
                        false
                    });
                }
            }
        }
    }
}

/// One declaration-owned inventory for annotation-bearing type parameters. Direct members inherit
/// their containing class's already-built classifier scope; local functions are discovered once from
/// the AST's shared expression inventory instead of adding origin-specific resolver call sites.
pub(in crate::resolve) fn resolve_declaration_type_parameter_annotation_inventory(
    file: &File,
    declaration: DeclId,
    class_names: &ClassNames,
    resolved: &mut HashMap<(u32, u32, u32), TypeName>,
    resolution: &mut DeclarationAnnotationResolution<'_>,
    diags: &mut DiagSink,
    include_local_bodies: bool,
) {
    let mut direct_owners = Vec::new();
    match file.decl(declaration) {
        Decl::Fun(function) => direct_owners.push(function.signature_span.lo),
        Decl::Property(property) => direct_owners.push(property.span.lo),
        Decl::Class(class) => {
            direct_owners.push(class.span.lo);
            direct_owners.extend(
                class
                    .methods
                    .iter()
                    .map(|function| function.signature_span.lo),
            );
            direct_owners.extend(class.body_props.iter().map(|property| property.span.lo));
        }
    }
    for declaration_start in direct_owners {
        resolve_declaration_type_parameter_annotations(
            file,
            declaration_start,
            class_names,
            resolved,
            resolution,
            diags,
        );
    }

    if !include_local_bodies {
        return;
    }
    let mut roots = Vec::new();
    file.any_decl_expr(declaration, &mut |root| {
        roots.push(root);
        false
    });
    if let Decl::Class(class) = file.decl(declaration) {
        let entry_roots = class
            .enum_entries
            .iter()
            .flat_map(enum_entry_expression_roots)
            .collect::<std::collections::HashSet<_>>();
        roots.retain(|root| !entry_roots.contains(root));
    }
    resolve_local_type_parameter_annotation_inventory(
        file,
        roots,
        class_names,
        resolved,
        resolution,
        diags,
    );
}

/// One enum entry together with the class that declares it. An entry's own classifier identity is
/// `Class.Entry`, so neither half answers a lookup alone.
pub(in crate::resolve) struct DeclaredEnumEntry<'a> {
    pub(in crate::resolve) class: &'a ClassDecl,
    pub(in crate::resolve) entry: &'a crate::ast::AstEnumEntry,
}

pub(in crate::resolve) fn resolve_enum_entry_type_parameter_annotation_inventory(
    file: &File,
    declared: DeclaredEnumEntry<'_>,
    class_names: &ClassNames,
    resolved: &mut HashMap<(u32, u32, u32), TypeName>,
    resolution: &mut DeclarationAnnotationResolution<'_>,
    diags: &mut DiagSink,
    include_local_bodies: bool,
) {
    let DeclaredEnumEntry { class, entry } = declared;
    let mut entry_class_names = class_names.clone();
    let entry_owner = type_name(&class_internal(
        file,
        &format!("{}.{}", class.name, entry.name),
    ));
    extend_lexical_nested_classifier_names(
        file,
        &HashMap::from([(entry_owner, 0)]),
        &mut entry_class_names,
    );
    for declaration_start in entry
        .methods
        .iter()
        .map(|function| function.signature_span.lo)
        .chain(entry.props.iter().map(|property| property.span.lo))
    {
        resolve_declaration_type_parameter_annotations(
            file,
            declaration_start,
            &entry_class_names,
            resolved,
            resolution,
            diags,
        );
    }
    if include_local_bodies {
        resolve_local_type_parameter_annotation_inventory(
            file,
            enum_entry_expression_roots(entry),
            &entry_class_names,
            resolved,
            resolution,
            diags,
        );
    }
}
