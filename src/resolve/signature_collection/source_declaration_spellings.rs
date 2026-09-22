//! The source spellings signature collection needs before any name is bound.
//!
//! A supertype, a bound, or a visibility suppression is written as text and must be inventoried
//! before the type universe is complete. These spellings are lookup input only; nothing downstream
//! recovers a symbol from them.

use super::*;

/// Project `@Suppress` visibility flags onto stable declarations after annotation names have been
/// resolved. Signature solving runs without source AST ownership, so it consumes this compact fact;
/// source spellings and annotation expression ids do not cross the boundary.
pub(in crate::resolve) fn collect_stable_visibility_suppressions(
    table: &mut SymbolTable,
    headers: Option<&crate::fir::StreamedHeaderModule>,
) {
    let Some(headers) = headers else {
        return;
    };
    for stub in &headers.stubs {
        let mut suppressions = VisibilitySuppressions::default();
        for application in headers
            .file_visibility_suppressions(stub.source)
            .iter()
            .chain(headers.declaration_visibility_suppressions(stub.id))
        {
            let annotation = AnnotationRef {
                name: String::new(),
                span: application.annotation,
            };
            if !table
                .resolved_annotation(stub.source.raw(), &annotation)
                .is_some_and(|identity| identity == type_name("kotlin/Suppress"))
            {
                continue;
            }
            suppressions.invisible_reference |= application.invisible_reference;
            suppressions.invisible_member |= application.invisible_member;
            suppressions.optional_declaration_usage |= application.optional_declaration_usage;
        }
        if suppressions.has_any() {
            table
                .declaration_visibility_suppressions
                .insert(stub.id, suppressions);
        }
    }
}

/// Record the SOURCE SPELLING of every declared type in the module, for `@Metadata`'s
/// `Type.abbreviated_type` (see [`crate::spelling`]).
///
/// This is a standalone pass rather than an extra output threaded through signature collection.
/// Collection is the hot path and its `Signature`s are consumed by resolution, inference, and the
/// checker — none of which may see a spelling. Emission is the only consumer, it addresses
/// declarations by the same AST coordinates used here, and a whole module that names no `typealias`
/// leaves both tables empty.
pub(in crate::resolve) fn collect_declared_spellings(
    table: &mut SymbolTable,
    files: &[File],
    file_class_names: &[ClassNames],
) {
    // Cloned once: the pass reads these while inserting into `table`, and a declaration's spelling
    // never depends on another declaration's.
    let expansions = &table.alias_expansion_spellings.clone();
    for (file_index, file) in files.iter().enumerate() {
        let file_index = file_index as u32;
        let names = &file_class_names[file_index as usize];
        // Spellings the parse seam parked when it expanded this file's own aliases away.
        let spellings = &file.alias_spellings;
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    let scope = spelling_scope(&f.type_params);
                    let spellings = crate::spelling::DeclaredSpellings {
                        superclass: crate::spelling::Spelled::default(),
                        ret: f
                            .ret
                            .as_ref()
                            .map(|r| spelling_of_ref(r, names, &scope, expansions, spellings))
                            .unwrap_or_default(),
                        params: f
                            .params
                            .iter()
                            .map(|p| spelling_of_ref(&p.ty, names, &scope, expansions, spellings))
                            .collect(),
                        receiver: f
                            .receiver
                            .as_ref()
                            .map(|r| spelling_of_ref(r, names, &scope, expansions, spellings))
                            .unwrap_or_default(),
                        type_param_bounds: type_param_bound_spellings(
                            &f.type_params,
                            &f.type_param_bounds,
                            names,
                            &scope,
                            expansions,
                            spellings,
                        ),
                        supertypes: Vec::new(),
                    };
                    if !spellings.is_none() {
                        table.declared_spellings.insert((file_index, d), spellings);
                    }
                }
                Decl::Property(p) => {
                    let scope = spelling_scope(&p.type_params);
                    let spellings = crate::spelling::DeclaredSpellings {
                        ret: p
                            .ty
                            .as_ref()
                            .map(|r| spelling_of_ref(r, names, &scope, expansions, spellings))
                            .unwrap_or_default(),
                        receiver: p
                            .receiver
                            .as_ref()
                            .map(|r| spelling_of_ref(r, names, &scope, expansions, spellings))
                            .unwrap_or_default(),
                        type_param_bounds: type_param_bound_spellings(
                            &p.type_params,
                            &p.type_param_bounds,
                            names,
                            &scope,
                            expansions,
                            spellings,
                        ),
                        ..Default::default()
                    };
                    if !spellings.is_none() {
                        table.declared_spellings.insert((file_index, d), spellings);
                    }
                }
                Decl::Class(c) => {
                    let scope = spelling_scope(&c.type_params);
                    // The class HEADER: supertypes, primary-constructor parameters, and the class's
                    // own type-parameter bounds.
                    let header = crate::spelling::DeclaredSpellings {
                        params: c
                            .props
                            .iter()
                            .map(|p| spelling_of_ref(&p.ty, names, &scope, expansions, spellings))
                            .collect(),
                        // The declared SUPERCLASS is parked as a bare name rather than a `TypeRef`
                        // (`ClassDecl::base_class`), which also means the parse seam never rewrote
                        // it — the alias spelling is still there. It is recorded APART from the
                        // interfaces because the emitted supertype list reserves a leading slot for
                        // it only for a generic class; `DeclaredSpellings::supertype_spellings`
                        // does that alignment where both lists are in hand.
                        superclass: c
                            .base_class
                            .as_deref()
                            .filter(|base| *base != "Any")
                            .map(|base| {
                                spelling_of_ref(
                                    &base_class_type_ref(
                                        base,
                                        &c.base_type_args,
                                        c.base_class_span.unwrap_or(c.span),
                                    ),
                                    names,
                                    &scope,
                                    expansions,
                                    spellings,
                                )
                            })
                            .unwrap_or_default(),
                        supertypes: c
                            .supertypes
                            .iter()
                            .map(|t| spelling_of_ref(t, names, &scope, expansions, spellings))
                            .collect(),
                        type_param_bounds: type_param_bound_spellings(
                            &c.type_params,
                            &c.type_param_bounds,
                            names,
                            &scope,
                            expansions,
                            spellings,
                        ),
                        ..Default::default()
                    };
                    if !header.is_none() {
                        table.declared_spellings.insert((file_index, d), header);
                    }
                    // Members are keyed by the exact AST coordinate selection already hands to
                    // lowering, so the two sides cannot drift on overloads.
                    for (method, m) in c.methods.iter().enumerate() {
                        // A member's own type parameters shadow the class's.
                        let mut member_scope = spelling_scope(&c.type_params);
                        for name in &m.type_params {
                            member_scope.insert_binding(
                                name,
                                Ty::nullable(Ty::obj("kotlin/Any")),
                                Vec::new(),
                            );
                        }
                        let spellings = crate::spelling::DeclaredSpellings {
                            superclass: crate::spelling::Spelled::default(),
                            ret: m
                                .ret
                                .as_ref()
                                .map(|r| {
                                    spelling_of_ref(r, names, &member_scope, expansions, spellings)
                                })
                                .unwrap_or_default(),
                            params: m
                                .params
                                .iter()
                                .map(|p| {
                                    spelling_of_ref(
                                        &p.ty,
                                        names,
                                        &member_scope,
                                        expansions,
                                        spellings,
                                    )
                                })
                                .collect(),
                            receiver: m
                                .receiver
                                .as_ref()
                                .map(|r| {
                                    spelling_of_ref(r, names, &member_scope, expansions, spellings)
                                })
                                .unwrap_or_default(),
                            type_param_bounds: type_param_bound_spellings(
                                &m.type_params,
                                &m.type_param_bounds,
                                names,
                                &member_scope,
                                expansions,
                                spellings,
                            ),
                            supertypes: Vec::new(),
                        };
                        if !spellings.is_none() {
                            table.member_spellings.insert(
                                crate::libraries::SourceMember::Class {
                                    file: file_index,
                                    owner: d.0,
                                    method: method as u32,
                                },
                                spellings,
                            );
                        }
                    }
                    // `SourceMember::ClassProperty` numbers the primary-constructor properties
                    // first and the body properties after them, so both share one index space.
                    for (property, p) in c.props.iter().enumerate() {
                        let spelling = spelling_of_ref(&p.ty, names, &scope, expansions, spellings);
                        if !spelling.is_none() {
                            table.member_spellings.insert(
                                crate::libraries::SourceMember::ClassProperty {
                                    file: file_index,
                                    owner: d.0,
                                    property: property as u32,
                                },
                                crate::spelling::DeclaredSpellings {
                                    ret: spelling,
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    for (body_property, p) in c.body_props.iter().enumerate() {
                        let property = c.props.len() + body_property;
                        // A body property is declared inside the class's type-parameter scope. Its
                        // own parameters (possible on an extension property) are the inner rung and
                        // therefore shadow class parameters with the same source spelling. Alias
                        // abbreviations need this complete scope too: in
                        // `class C<T> { val value: Alias<T> }`, the as-written alias argument must
                        // encode `T`, even when the alias expansion does not use that parameter.
                        let mut member_scope = spelling_scope(&c.type_params);
                        for name in &p.type_params {
                            member_scope.insert_binding(
                                name,
                                Ty::nullable(Ty::obj("kotlin/Any")),
                                Vec::new(),
                            );
                        }
                        let spellings = crate::spelling::DeclaredSpellings {
                            ret: p
                                .ty
                                .as_ref()
                                .map(|r| {
                                    spelling_of_ref(r, names, &member_scope, expansions, spellings)
                                })
                                .unwrap_or_default(),
                            receiver: p
                                .receiver
                                .as_ref()
                                .map(|r| {
                                    spelling_of_ref(r, names, &member_scope, expansions, spellings)
                                })
                                .unwrap_or_default(),
                            ..Default::default()
                        };
                        if !spellings.is_none() {
                            table.member_spellings.insert(
                                crate::libraries::SourceMember::ClassProperty {
                                    file: file_index,
                                    owner: d.0,
                                    property: property as u32,
                                },
                                spellings,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Rebuild the declared base class as a [`TypeRef`] so it can be spelled like any other declared
/// type. `ClassDecl` stores it split into a bare name plus its type arguments, and only a whole
/// reference can carry an alias with its as-written arguments.
pub(in crate::resolve) fn base_class_type_ref(
    base: &str,
    type_args: &[TypeRef],
    span: Span,
) -> TypeRef {
    TypeRef {
        name: base.to_string(),
        flags: TrFlags::default(),
        arg: None,
        targs: type_args.to_vec(),
        span,
        fun_params: Vec::new(),
        fun_context_count: 0,
    }
}

/// A name-only type-parameter scope: [`spelling_of_ref`] asks a `TParams` exactly one question —
/// does this spelling name a type parameter (and therefore never an alias) — so the bounds it
/// carries are irrelevant here.
pub(in crate::resolve) fn spelling_scope(type_params: &[String]) -> TParams {
    TParams::from_bindings(
        type_params
            .iter()
            .map(|name| (name.clone(), Ty::nullable(Ty::obj("kotlin/Any")))),
    )
}

/// Spellings of each declared type parameter's upper bounds, in declaration order, so
/// `<T : Cargo>` records `Cargo` on the bound `Type` the way kotlinc does.
fn type_param_bound_spellings(
    type_params: &[String],
    bounds: &[(String, TypeRef)],
    names: &ClassNames,
    scope: &TParams,
    expansions: &HashMap<TypeName, (Spelled, Vec<String>, Ty)>,
    spellings: &HashMap<Span, TypeRef>,
) -> Vec<Vec<Spelled>> {
    type_params
        .iter()
        .map(|parameter| {
            bounds
                .iter()
                .filter(|(name, _)| name == parameter)
                .map(|(_, bound)| spelling_of_ref(bound, names, scope, expansions, spellings))
                .collect()
        })
        .collect()
}
