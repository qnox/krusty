//! Which executable declaration owns a parser-hoisted local or anonymous classifier.
//!
//! A classifier written inside a body belongs to the executable that introduces it: a property
//! initializer or accessor, a function, or a class initializer block. The owner is the same stable
//! anchor that declaration's own stub interns, so both sides name one declaration.

use super::*;

fn anonymous_property_owner(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    property: &PropDecl,
    owner: Option<DeclarationId>,
    sibling: u32,
    anonymous: Span,
) -> Option<(u32, DeclarationId)> {
    let contains = |outer: Span| outer.lo <= anonymous.lo && anonymous.hi <= outer.hi;
    let body_range = |body: &FunBody| match body {
        FunBody::Expr(expression) | FunBody::Block(expression) => file.expr_span(*expression),
        FunBody::None => None,
    };
    if let Some(range) = property
        .getter
        .as_ref()
        .and_then(body_range)
        .filter(|range| contains(*range))
    {
        let property_id = ids.intern(DeclarationAnchor {
            source,
            range: property.span,
            owner,
            kind: DeclarationKind::Property,
            sibling,
        });
        return Some((
            range.hi - range.lo,
            ids.intern(DeclarationAnchor {
                source,
                range,
                owner: Some(property_id),
                kind: DeclarationKind::Accessor,
                sibling: 0,
            }),
        ));
    }
    if let Some(range) = property
        .setter
        .as_ref()
        .and_then(|setter| setter.body.as_ref())
        .and_then(body_range)
        .filter(|range| contains(*range))
    {
        let property_id = ids.intern(DeclarationAnchor {
            source,
            range: property.span,
            owner,
            kind: DeclarationKind::Property,
            sibling,
        });
        return Some((
            range.hi - range.lo,
            ids.intern(DeclarationAnchor {
                source,
                range,
                owner: Some(property_id),
                kind: DeclarationKind::Accessor,
                sibling: 1,
            }),
        ));
    }
    property
        .init
        .into_iter()
        .chain(property.delegate)
        .filter_map(|expression| file.expr_span(expression))
        .filter(|range| contains(*range))
        .min_by_key(|range| range.hi - range.lo)
        .map(|range| {
            let property_id = ids.intern(DeclarationAnchor {
                source,
                range: property.span,
                owner,
                kind: DeclarationKind::Property,
                sibling,
            });
            (range.hi - range.lo, property_id)
        })
}

fn property_contains_anonymous(file: &File, property: &PropDecl, anonymous: Span) -> bool {
    let contains = |outer: Span| outer.lo <= anonymous.lo && anonymous.hi <= outer.hi;
    let body_range = |body: &FunBody| match body {
        FunBody::Expr(expression) | FunBody::Block(expression) => file.expr_span(*expression),
        FunBody::None => None,
    };
    property
        .getter
        .as_ref()
        .and_then(body_range)
        .is_some_and(contains)
        || property
            .setter
            .as_ref()
            .and_then(|setter| setter.body.as_ref())
            .and_then(body_range)
            .is_some_and(contains)
        || property
            .init
            .into_iter()
            .chain(property.delegate)
            .filter_map(|expression| file.expr_span(expression))
            .any(contains)
}

pub(super) fn local_executable_owner(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    declaration: DeclId,
    classifier_ids: &std::collections::HashMap<DeclId, DeclarationId>,
) -> Option<DeclarationId> {
    let local = match file.decl(declaration) {
        Decl::Class(class) => class.span,
        Decl::Fun(_) | Decl::Property(_) => return None,
    };
    let mut property_candidates = Vec::new();
    for (sibling, candidate) in file.decls.iter().copied().enumerate() {
        match file.decl(candidate) {
            Decl::Property(property) => {
                if let Some(candidate) = anonymous_property_owner(
                    file,
                    source,
                    ids,
                    property,
                    None,
                    u32::try_from(sibling).expect("too many file declarations"),
                    local,
                ) {
                    property_candidates.push(candidate);
                }
            }
            Decl::Class(class) => {
                if !class
                    .body_props
                    .iter()
                    .any(|property| property_contains_anonymous(file, property, local))
                {
                    continue;
                }
                let class_id = classifier_ids
                    .get(&candidate)
                    .copied()
                    .or_else(|| classifier_identity(file, source, ids, candidate));
                let Some(class_id) = class_id else {
                    continue;
                };
                for (property_sibling, property) in class.body_props.iter().enumerate() {
                    if let Some(candidate) = anonymous_property_owner(
                        file,
                        source,
                        ids,
                        property,
                        Some(class_id),
                        u32::try_from(property_sibling).expect("too many class properties"),
                        local,
                    ) {
                        property_candidates.push(candidate);
                    }
                }
            }
            Decl::Fun(_) => {}
        }
    }
    if let Some((_, owner)) = property_candidates
        .into_iter()
        .min_by_key(|(length, _)| *length)
    {
        return Some(owner);
    }

    let contains = |outer: Span| outer.lo <= local.lo && local.hi <= outer.hi;
    let mut function_candidates = Vec::new();
    for (sibling, candidate) in file.decls.iter().copied().enumerate() {
        match file.decl(candidate) {
            Decl::Fun(function) if contains(function.span) => {
                function_candidates.push((
                    function.span.hi - function.span.lo,
                    ids.intern(DeclarationAnchor {
                        source,
                        range: function.span,
                        owner: None,
                        kind: DeclarationKind::Function,
                        sibling: u32::try_from(sibling).expect("too many file declarations"),
                    }),
                ));
            }
            Decl::Class(class) => {
                let containing_methods = class
                    .methods
                    .iter()
                    .enumerate()
                    .filter(|(_, function)| contains(function.span))
                    .collect::<Vec<_>>();
                // An `init` block is an executable of its own: the stub inventory interns it with
                // the block's range and its position in the class's initialization order.
                let containing_initializers = class
                    .init_order
                    .iter()
                    .enumerate()
                    .filter_map(|(order, step)| match step {
                        ClassInit::Block(block) => Some((order, file.expr_span(*block)?)),
                        ClassInit::PropInit(_) => None,
                    })
                    .filter(|(_, range)| contains(*range))
                    .collect::<Vec<_>>();
                if containing_methods.is_empty() && containing_initializers.is_empty() {
                    continue;
                }
                let class_id = classifier_ids
                    .get(&candidate)
                    .copied()
                    .or_else(|| classifier_identity(file, source, ids, candidate));
                let Some(class_id) = class_id else {
                    continue;
                };
                for (order, range) in containing_initializers {
                    function_candidates.push((
                        range.hi - range.lo,
                        ids.intern(DeclarationAnchor {
                            source,
                            range,
                            owner: Some(class_id),
                            kind: DeclarationKind::Initializer,
                            sibling: u32::try_from(order).expect("too many class initializers"),
                        }),
                    ));
                }
                for (method_sibling, function) in containing_methods {
                    function_candidates.push((
                        function.span.hi - function.span.lo,
                        ids.intern(DeclarationAnchor {
                            source,
                            range: function.span,
                            owner: Some(class_id),
                            kind: DeclarationKind::Function,
                            sibling: u32::try_from(method_sibling).expect("too many class methods"),
                        }),
                    ));
                }
            }
            Decl::Fun(_) | Decl::Property(_) => {}
        }
    }
    if let Some((_, owner)) = function_candidates
        .into_iter()
        .min_by_key(|(length, _)| *length)
    {
        return Some(owner);
    }

    enclosing_function_identity(
        file,
        source,
        ids,
        classifier_ids,
        *file
            .anonymous_object_enclosing_functions
            .get(&declaration)?,
    )
}

/// The stable identity of a source function the parser recorded as an exact lexical enclosure.
///
/// The only way from [`crate::ast::AnonymousEnclosingFunction`] to a declaration id: it interns the
/// same anchor `function_stub` does, so both sides name one declaration. Every consumer of that
/// edge goes through here rather than matching on the variants again.
fn enclosing_function_identity(
    file: &File,
    source: SourceFileId,
    ids: &mut DeclarationIds,
    classifier_ids: &std::collections::HashMap<DeclId, DeclarationId>,
    enclosing: crate::ast::AnonymousEnclosingFunction,
) -> Option<DeclarationId> {
    match enclosing {
        crate::ast::AnonymousEnclosingFunction::TopLevel(function) => {
            let Decl::Fun(function_decl) = file.decl(function) else {
                return None;
            };
            let sibling = u32::try_from(
                file.decls
                    .iter()
                    .position(|candidate| *candidate == function)?,
            )
            .ok()?;
            Some(ids.intern(DeclarationAnchor {
                source,
                range: function_decl.span,
                owner: None,
                kind: DeclarationKind::Function,
                sibling,
            }))
        }
        crate::ast::AnonymousEnclosingFunction::Member { class, method } => {
            let owner = classifier_ids
                .get(&class)
                .copied()
                .or_else(|| classifier_identity(file, source, ids, class))?;
            let Decl::Class(class_decl) = file.decl(class) else {
                return None;
            };
            let function = class_decl.methods.get(method as usize)?;
            Some(ids.intern(DeclarationAnchor {
                source,
                range: function.span,
                owner: Some(owner),
                kind: DeclarationKind::Function,
                sibling: method,
            }))
        }
    }
}
