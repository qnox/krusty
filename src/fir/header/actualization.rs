//! Which `expect` headers this module's `actual` declarations answer for.
//!
//! The match is made from compact Pass-1 facts alone — it decides which declarations survive into
//! signature collection, so it runs before any of them are resolved. What it compares is still an
//! IDENTITY rather than a spelling: a classifier is the one each file's package and imports name,
//! and a type parameter is matched by position.

use super::*;

/// Match top-level multiplatform headers using only compact Pass-1 facts. The returned stable ids
/// identify expect declarations shadowed by a same-package, same-shape non-expect declaration or
/// type alias. No parser declaration id participates in the match.
pub fn matched_expect_declarations(
    headers: &StreamedHeaderModule,
) -> std::collections::HashSet<DeclarationId> {
    actualized_declaration_pairs(headers)
        .into_iter()
        .filter_map(|pair| {
            headers
                .declarations
                .anchor(pair.expect)
                .is_some_and(|anchor| anchor.owner.is_none())
                .then_some(pair.expect)
        })
        .collect()
}

/// One stable expect declaration and the actual declaration that replaces it. Descendant pairs are
/// included: an `expect class A { class B { fun f(...) } }` actualizes `A`, `A.B`, and `f` as one
/// semantic subtree even though only `A` carries the source `expect` modifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActualizedDeclarationPair {
    pub expect: DeclarationId,
    pub actual: DeclarationId,
}

/// What one module's `expect` headers were matched with.
#[derive(Clone, Debug, Default)]
pub struct Actualization {
    pub pairs: Vec<ActualizedDeclarationPair>,
    /// Top-level implementations that did NOT write `actual`.
    ///
    /// A declaration sharing an `expect`'s package, kind, name, receiver and arity is the
    /// implementation that header was written for, and the reference compiler says so at the
    /// declaration's own name rather than reporting the `expect` as unfilled — the coincidence is
    /// an error about the IMPLEMENTATION. The modifier is otherwise unrecoverable: it is gone by
    /// the time headers are compacted, and a declaration's shape says nothing about whether it
    /// claimed to implement anything.
    pub unmarked: Vec<DeclarationId>,
    /// Top-level `expect` declarations an implementation was WRITTEN for and none matched.
    ///
    /// A candidate sharing the header's package, kind, name, receiver and arity is an attempt at
    /// implementing it; when its input shapes disagree the reference compiler reports the
    /// incompatibility on the implementation and says nothing about the header. Reporting the
    /// header as unactualized as well names the same mismatch twice, from the side that did not
    /// get it wrong.
    pub incompatible: std::collections::HashSet<DeclarationId>,
}

/// [`Actualization::pairs`] alone, for callers that need no more.
pub fn actualized_declaration_pairs(
    headers: &StreamedHeaderModule,
) -> Vec<ActualizedDeclarationPair> {
    actualization(headers).pairs
}

/// Match actualized declaration subtrees using compact Pass-1 headers only. This is also the
/// authority for expect-owned default expressions: callers publish their presence on `actual`, but
/// retain `expect` as the stable provider identity for Pass-2 checking.
pub fn actualization(headers: &StreamedHeaderModule) -> Actualization {
    type Key = (String, u8, String, bool, usize);
    /// One `expect` classifier actualized by a `typealias`: the classifier's QUALIFIED identity,
    /// the alias's target type, and the file the target is written in — which is the scope that
    /// target's own spelling resolves in.
    type ActualizedAlias = (String, HeaderTypeId, SourceFileId);

    /// Classifier identity for compact headers.
    ///
    /// Actualization runs before signatures are resolved, so the only identity available is the one
    /// the module's own declarations and each file's imports establish. It is still an IDENTITY
    /// rather than a spelling: `plib.model.Tally` written out and `Tally` under
    /// `import plib.model.Tally` are the same classifier, and `Tally` imported from `plib.left` and
    /// from `plib.right` are two.
    struct ClassifierScopes {
        /// Qualified names of every classifier and type alias this module declares.
        declared: std::collections::HashSet<String>,
        /// Per source file: the package it declares, and the explicit imports it wrote, keyed by
        /// the simple name each brings into scope.
        files: std::collections::HashMap<u32, (String, std::collections::HashMap<String, String>)>,
    }

    impl ClassifierScopes {
        fn of(headers: &StreamedHeaderModule) -> Self {
            let mut declared = std::collections::HashSet::new();
            for stub in &headers.stubs {
                if !matches!(
                    stub.kind,
                    DeclarationKind::Classifier | DeclarationKind::TypeAlias
                ) {
                    continue;
                }
                let Some(spelling) = stub
                    .lookup_name
                    .and_then(|name| headers.lookup_names.get(name))
                else {
                    continue;
                };
                let package = headers
                    .sources
                    .get(stub.source)
                    .map(|source| source.package.render().replace('/', "."))
                    .unwrap_or_default();
                let spelling = spelling.replace('$', ".");
                declared.insert(if package.is_empty() {
                    spelling
                } else {
                    format!("{package}.{spelling}")
                });
            }
            let mut files = std::collections::HashMap::new();
            for (index, inventoried) in headers.inventoried.iter().enumerate() {
                if !inventoried {
                    continue;
                }
                let source = SourceFileId::from_raw(index as u32);
                let package = headers
                    .sources
                    .get(source)
                    .map(|entry| entry.package.render().replace('/', "."))
                    .unwrap_or_default();
                let mut imports = std::collections::HashMap::new();
                for root in headers.detached_type_roots(source) {
                    let Some(ty) = headers.syntax.ty(root) else {
                        continue;
                    };
                    if !ty.flags.is_import() {
                        continue;
                    }
                    let HeaderTypeKind::Classifier { detail, .. } = ty.kind else {
                        continue;
                    };
                    let Some(detail) = headers.syntax.classifier_type(detail) else {
                        continue;
                    };
                    let segments = headers
                        .syntax
                        .type_path(detail.path)
                        .iter()
                        .filter_map(|segment| headers.lookup_names.get(*segment))
                        .collect::<Vec<_>>();
                    let Some(last) = segments.last() else {
                        continue;
                    };
                    imports.insert((*last).to_string(), segments.join("."));
                }
                files.insert(index as u32, (package, imports));
            }
            Self { declared, files }
        }

        /// The classifier a written path names, as seen from `source`.
        ///
        /// A path nothing in the module or the file's imports claims keeps its own spelling: that
        /// is a canonical form both sides of a comparison reach the same way, not a fallback to
        /// the text — `Int` against `Int` is one classifier however the module spells it.
        fn resolve(&self, source: SourceFileId, segments: &[&str]) -> String {
            let written = segments.join(".");
            let Some((package, imports)) = self.files.get(&source.raw()) else {
                return written;
            };
            if let Some(first) = segments.first() {
                if let Some(qualified) = imports.get(*first) {
                    let rest = segments[1..].join(".");
                    return if rest.is_empty() {
                        qualified.clone()
                    } else {
                        format!("{qualified}.{rest}")
                    };
                }
            }
            if !package.is_empty() {
                let qualified = format!("{package}.{written}");
                if self.declared.contains(&qualified) {
                    return qualified;
                }
            }
            written
        }
    }

    /// The two files one comparison reads its spellings in, and the module scope both resolve
    /// against.
    #[derive(Clone, Copy)]
    struct MatchScope<'a> {
        scopes: &'a ClassifierScopes,
        expect: SourceFileId,
        candidate: SourceFileId,
    }

    impl MatchScope<'_> {
        fn swapped(self, expect: SourceFileId) -> Self {
            Self { expect, ..self }
        }
    }

    fn type_flags_match(left: HeaderTypeFlags, right: HeaderTypeFlags) -> bool {
        left.nullable() == right.nullable()
            && left.definitely_non_null() == right.definitely_non_null()
            && left.function_receiver() == right.function_receiver()
            && left.suspend_function() == right.suspend_function()
            && left.in_projection() == right.in_projection()
            && left.out_projection() == right.out_projection()
            && left.star_projection() == right.star_projection()
    }

    fn type_shape_matches(
        headers: &StreamedHeaderModule,
        expect: HeaderTypeId,
        candidate: HeaderTypeId,
        type_parameters: &[(LookupNameId, LookupNameId)],
        actualized_aliases: &[ActualizedAlias],
        scope: MatchScope<'_>,
    ) -> bool {
        let candidate_id = candidate;
        let (Some(expect), Some(candidate)) =
            (headers.syntax.ty(expect), headers.syntax.ty(candidate_id))
        else {
            return false;
        };
        let direct = type_flags_match(expect.flags, candidate.flags)
            && match (expect.kind, candidate.kind) {
                (
                    HeaderTypeKind::Classifier {
                        detail: expect_detail,
                        abbreviated_argument: expect_abbreviated,
                    },
                    HeaderTypeKind::Classifier {
                        detail: candidate_detail,
                        abbreviated_argument: candidate_abbreviated,
                    },
                ) => {
                    let (Some(expect_detail), Some(candidate_detail)) = (
                        headers.syntax.classifier_type(expect_detail),
                        headers.syntax.classifier_type(candidate_detail),
                    ) else {
                        return false;
                    };
                    let expect_path = headers.syntax.type_path(expect_detail.path);
                    let candidate_path = headers.syntax.type_path(candidate_detail.path);
                    // A TYPE PARAMETER is matched positionally — the two declarations may name
                    // theirs differently — and everything else by the classifier it identifies,
                    // which is what each file's package and imports say and not what it spelled.
                    let path_matches = match (expect_path, candidate_path) {
                        ([expect], [candidate])
                            if type_parameters
                                .iter()
                                .any(|(parameter, _)| parameter == expect) =>
                        {
                            type_parameters
                                .iter()
                                .find(|(parameter, _)| parameter == expect)
                                .is_some_and(|(_, parameter)| parameter == candidate)
                        }
                        _ => {
                            let segments = |path: &[LookupNameId]| {
                                path.iter()
                                    .filter_map(|segment| headers.lookup_names.get(*segment))
                                    .collect::<Vec<_>>()
                            };
                            scope.scopes.resolve(scope.expect, &segments(expect_path))
                                == scope
                                    .scopes
                                    .resolve(scope.candidate, &segments(candidate_path))
                        }
                    };
                    path_matches
                        && headers
                            .syntax
                            .type_operands(expect_detail.arguments)
                            .iter()
                            .zip(headers.syntax.type_operands(candidate_detail.arguments))
                            .all(|(&expect, &candidate)| {
                                type_shape_matches(
                                    headers,
                                    expect,
                                    candidate,
                                    type_parameters,
                                    actualized_aliases,
                                    scope,
                                )
                            })
                        && headers.syntax.type_operands(expect_detail.arguments).len()
                            == headers
                                .syntax
                                .type_operands(candidate_detail.arguments)
                                .len()
                        && match (expect_abbreviated, candidate_abbreviated) {
                            (Some(expect), Some(candidate)) => type_shape_matches(
                                headers,
                                expect,
                                candidate,
                                type_parameters,
                                actualized_aliases,
                                scope,
                            ),
                            (None, None) => true,
                            (Some(_), None) | (None, Some(_)) => false,
                        }
                }
                (
                    HeaderTypeKind::Function {
                        parameters: expect_parameters,
                        result: expect_result,
                        context_count: expect_context_count,
                    },
                    HeaderTypeKind::Function {
                        parameters: candidate_parameters,
                        result: candidate_result,
                        context_count: candidate_context_count,
                    },
                ) => {
                    let expect_parameters = headers.syntax.type_operands(expect_parameters);
                    let candidate_parameters = headers.syntax.type_operands(candidate_parameters);
                    expect_context_count == candidate_context_count
                        && expect_parameters.len() == candidate_parameters.len()
                        && expect_parameters.iter().zip(candidate_parameters).all(
                            |(&expect, &candidate)| {
                                type_shape_matches(
                                    headers,
                                    expect,
                                    candidate,
                                    type_parameters,
                                    actualized_aliases,
                                    scope,
                                )
                            },
                        )
                        && match (expect_result, candidate_result) {
                            (Some(expect), Some(candidate)) => type_shape_matches(
                                headers,
                                expect,
                                candidate,
                                type_parameters,
                                actualized_aliases,
                                scope,
                            ),
                            (None, None) => true,
                            (Some(_), None) | (None, Some(_)) => false,
                        }
                }
                (HeaderTypeKind::Classifier { .. }, HeaderTypeKind::Function { .. })
                | (HeaderTypeKind::Function { .. }, HeaderTypeKind::Classifier { .. }) => false,
            };
        if direct {
            return true;
        }
        // An `expect class` actualized by a `typealias` is the alias's TARGET on the platform
        // side. Which classifier the expect side named is its resolved identity, not its simple
        // name: two `Carried`s in different packages are two classifiers, and only one of them
        // may have been actualized.
        if let HeaderTypeKind::Classifier { detail, .. } = expect.kind {
            if let Some(detail) = headers.syntax.classifier_type(detail) {
                let written = headers
                    .syntax
                    .type_path(detail.path)
                    .iter()
                    .filter_map(|segment| headers.lookup_names.get(*segment))
                    .collect::<Vec<_>>();
                let identity = scope.scopes.resolve(scope.expect, &written);
                let mut aliases = actualized_aliases
                    .iter()
                    .filter(|(name, _, _)| *name == identity);
                if let (Some((_, target, source)), None) = (aliases.next(), aliases.next()) {
                    return type_shape_matches(
                        headers,
                        *target,
                        candidate_id,
                        type_parameters,
                        actualized_aliases,
                        scope.swapped(*source),
                    );
                }
            }
        }
        false
    }

    fn callable_parameter_shapes_match(
        headers: &StreamedHeaderModule,
        expect: DeclarationId,
        candidate: DeclarationId,
        actualized_aliases: &[ActualizedAlias],
        scopes: &ClassifierScopes,
    ) -> bool {
        let (Some(expect_source), Some(candidate_source)) = (
            headers
                .declarations
                .anchor(expect)
                .map(|anchor| anchor.source),
            headers
                .declarations
                .anchor(candidate)
                .map(|anchor| anchor.source),
        ) else {
            return false;
        };
        let scope = MatchScope {
            scopes,
            expect: expect_source,
            candidate: candidate_source,
        };
        let (
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Callable {
                        receiver: expect_receiver,
                        parameters: expect_parameters,
                        type_parameters: expect_type_parameters,
                        context_count: expect_context_count,
                        ..
                    },
                ..
            }),
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Callable {
                        receiver: candidate_receiver,
                        parameters: candidate_parameters,
                        type_parameters: candidate_type_parameters,
                        context_count: candidate_context_count,
                        ..
                    },
                ..
            }),
        ) = (
            headers.syntax.declaration(expect),
            headers.syntax.declaration(candidate),
        )
        else {
            return false;
        };
        let expect_type_parameters = headers.syntax.type_parameters(expect_type_parameters);
        let candidate_type_parameters = headers.syntax.type_parameters(candidate_type_parameters);
        if expect_type_parameters.len() != candidate_type_parameters.len()
            || expect_context_count != candidate_context_count
        {
            return false;
        }
        let own = expect_type_parameters
            .iter()
            .zip(candidate_type_parameters)
            .map(|(expect, candidate)| (expect.name, candidate.name))
            .collect::<Vec<_>>();
        let Some(type_parameters) = positional_type_parameters(headers, expect, candidate, &own)
        else {
            return false;
        };
        let receiver_matches = match (expect_receiver, candidate_receiver) {
            (Some(expect), Some(candidate)) => type_shape_matches(
                headers,
                expect,
                candidate,
                &type_parameters,
                actualized_aliases,
                scope,
            ),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        let expect_parameters = headers.syntax.parameters(expect_parameters);
        let candidate_parameters = headers.syntax.parameters(candidate_parameters);
        receiver_matches
            && expect_parameters.len() == candidate_parameters.len()
            && expect_parameters
                .iter()
                .zip(candidate_parameters)
                .all(|(expect, candidate)| {
                    expect.flags.is_vararg() == candidate.flags.is_vararg()
                        && type_shape_matches(
                            headers,
                            expect.ty,
                            candidate.ty,
                            &type_parameters,
                            actualized_aliases,
                            scope,
                        )
                })
    }

    fn property_input_shapes_match(
        headers: &StreamedHeaderModule,
        expect: DeclarationId,
        candidate: DeclarationId,
        actualized_aliases: &[ActualizedAlias],
        scopes: &ClassifierScopes,
    ) -> bool {
        let (Some(expect_source), Some(candidate_source)) = (
            headers
                .declarations
                .anchor(expect)
                .map(|anchor| anchor.source),
            headers
                .declarations
                .anchor(candidate)
                .map(|anchor| anchor.source),
        ) else {
            return false;
        };
        let scope = MatchScope {
            scopes,
            expect: expect_source,
            candidate: candidate_source,
        };
        let (
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Property {
                        receiver: expect_receiver,
                        context_parameters: expect_context,
                        type_parameters: expect_type_parameters,
                        mutable: expect_mutable,
                        ..
                    },
                ..
            }),
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Property {
                        receiver: candidate_receiver,
                        context_parameters: candidate_context,
                        type_parameters: candidate_type_parameters,
                        mutable: candidate_mutable,
                        ..
                    },
                ..
            }),
        ) = (
            headers.syntax.declaration(expect),
            headers.syntax.declaration(candidate),
        )
        else {
            return false;
        };
        if expect_mutable && !candidate_mutable {
            return false;
        }
        let expect_type_parameters = headers.syntax.type_parameters(expect_type_parameters);
        let candidate_type_parameters = headers.syntax.type_parameters(candidate_type_parameters);
        if expect_type_parameters.len() != candidate_type_parameters.len() {
            return false;
        }
        let own = expect_type_parameters
            .iter()
            .zip(candidate_type_parameters)
            .map(|(expect, candidate)| (expect.name, candidate.name))
            .collect::<Vec<_>>();
        let Some(type_parameters) = positional_type_parameters(headers, expect, candidate, &own)
        else {
            return false;
        };
        let receiver_matches = match (expect_receiver, candidate_receiver) {
            (Some(expect), Some(candidate)) => type_shape_matches(
                headers,
                expect,
                candidate,
                &type_parameters,
                actualized_aliases,
                scope,
            ),
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        };
        let expect_context = headers.syntax.parameters(expect_context);
        let candidate_context = headers.syntax.parameters(candidate_context);
        receiver_matches
            && expect_context.len() == candidate_context.len()
            && expect_context
                .iter()
                .zip(candidate_context)
                .all(|(expect, candidate)| {
                    type_shape_matches(
                        headers,
                        expect.ty,
                        candidate.ty,
                        &type_parameters,
                        actualized_aliases,
                        scope,
                    )
                })
    }

    /// The type parameters in scope from a declaration's OWNERS, outermost first.
    ///
    /// A member states its owner's type parameters by name, and the two sides of a pair may name
    /// theirs differently — `expect class A<B, C> { fun o(b: B): C }` against
    /// `actual class A<C, B> { actual fun o(b: C): B }` is one legal pair. They are matched by
    /// POSITION, exactly as a callable's own are, so the owner chain is collected here rather than
    /// each member's comparison starting from an empty scope.
    fn enclosing_type_parameters(
        headers: &StreamedHeaderModule,
        declaration: DeclarationId,
    ) -> Vec<LookupNameId> {
        let mut owners = Vec::new();
        let mut current = headers
            .declarations
            .anchor(declaration)
            .and_then(|anchor| anchor.owner);
        while let Some(owner) = current {
            owners.push(owner);
            current = headers
                .declarations
                .anchor(owner)
                .and_then(|anchor| anchor.owner);
        }
        owners
            .into_iter()
            .rev()
            .flat_map(|owner| {
                let parameters = match headers.syntax.declaration(owner).map(|value| value.kind) {
                    Some(HeaderDeclarationKind::Classifier {
                        type_parameters, ..
                    })
                    | Some(HeaderDeclarationKind::Callable {
                        type_parameters, ..
                    }) => type_parameters,
                    _ => return Vec::new(),
                };
                headers
                    .syntax
                    .type_parameters(parameters)
                    .iter()
                    .map(|parameter| parameter.name)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The positional type-parameter map for one pair: the owners' parameters, then the
    /// declaration's own.
    fn positional_type_parameters(
        headers: &StreamedHeaderModule,
        expect: DeclarationId,
        candidate: DeclarationId,
        own: &[(LookupNameId, LookupNameId)],
    ) -> Option<Vec<(LookupNameId, LookupNameId)>> {
        let expect_owners = enclosing_type_parameters(headers, expect);
        let candidate_owners = enclosing_type_parameters(headers, candidate);
        if expect_owners.len() != candidate_owners.len() {
            return None;
        }
        Some(
            expect_owners
                .into_iter()
                .zip(candidate_owners)
                .chain(own.iter().copied())
                .collect(),
        )
    }

    /// A constructor pair, compared by the parameter types both declare.
    ///
    /// A constructor is its own header kind: it carries no receiver and no type parameters of its
    /// own — the classifier's are in scope — so its parameters are the whole of its input shape.
    fn constructor_parameter_shapes_match(
        headers: &StreamedHeaderModule,
        expect: DeclarationId,
        candidate: DeclarationId,
        actualized_aliases: &[ActualizedAlias],
        scopes: &ClassifierScopes,
    ) -> bool {
        let (
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Constructor {
                        parameters: expect_parameters,
                        ..
                    },
                ..
            }),
            Some(HeaderDeclaration {
                kind:
                    HeaderDeclarationKind::Constructor {
                        parameters: candidate_parameters,
                        ..
                    },
                ..
            }),
        ) = (
            headers.syntax.declaration(expect),
            headers.syntax.declaration(candidate),
        )
        else {
            return false;
        };
        let (Some(expect_source), Some(candidate_source)) = (
            headers
                .declarations
                .anchor(expect)
                .map(|anchor| anchor.source),
            headers
                .declarations
                .anchor(candidate)
                .map(|anchor| anchor.source),
        ) else {
            return false;
        };
        let scope = MatchScope {
            scopes,
            expect: expect_source,
            candidate: candidate_source,
        };
        let Some(type_parameters) = positional_type_parameters(headers, expect, candidate, &[])
        else {
            return false;
        };
        let expect_parameters = headers.syntax.parameters(expect_parameters);
        let candidate_parameters = headers.syntax.parameters(candidate_parameters);
        expect_parameters.len() == candidate_parameters.len()
            && expect_parameters
                .iter()
                .zip(candidate_parameters)
                .all(|(expect, candidate)| {
                    expect.flags.is_vararg() == candidate.flags.is_vararg()
                        && type_shape_matches(
                            headers,
                            expect.ty,
                            candidate.ty,
                            &type_parameters,
                            actualized_aliases,
                            scope,
                        )
                })
    }

    /// The one candidate that answers for `expect`, or none.
    ///
    /// Every kind answers here. A callable and a property are decided by their INPUT shapes, read
    /// as classifier identities; the kinds that declare no inputs — a classifier, a type alias, an
    /// accessor, an enum entry — are decided by the coarse key that bucketed them, which is their
    /// whole identity under one owner. There is no lone-candidate fallback: accepting a single
    /// coarse candidate paired declarations whose shapes had already been compared and rejected.
    fn select_actual(
        headers: &StreamedHeaderModule,
        expect: &DeclarationStub,
        candidates: &[DeclarationId],
        actualized_aliases: &[ActualizedAlias],
        scopes: &ClassifierScopes,
    ) -> Option<DeclarationId> {
        let matching = candidates
            .iter()
            .copied()
            .filter(|candidate| match expect.kind {
                DeclarationKind::Function => callable_parameter_shapes_match(
                    headers,
                    expect.id,
                    *candidate,
                    actualized_aliases,
                    scopes,
                ),
                DeclarationKind::Constructor => constructor_parameter_shapes_match(
                    headers,
                    expect.id,
                    *candidate,
                    actualized_aliases,
                    scopes,
                ),
                DeclarationKind::Property => property_input_shapes_match(
                    headers,
                    expect.id,
                    *candidate,
                    actualized_aliases,
                    scopes,
                ),
                DeclarationKind::Classifier
                | DeclarationKind::TypeAlias
                | DeclarationKind::Accessor
                | DeclarationKind::Initializer
                | DeclarationKind::EnumEntry
                | DeclarationKind::Script => true,
            })
            .collect::<Vec<_>>();
        (matching.len() == 1).then(|| matching[0])
    }

    fn path(headers: &StreamedHeaderModule, range: LookupNameRange, separator: &str) -> String {
        headers
            .scopes
            .path(range)
            .iter()
            .filter_map(|name| headers.lookup_names.get(*name))
            .collect::<Vec<_>>()
            .join(separator)
    }

    fn key(headers: &StreamedHeaderModule, stub: &DeclarationStub) -> Option<Key> {
        let anchor = headers.declarations.anchor(stub.id)?;
        if anchor.owner.is_some() {
            return None;
        }
        let scope = headers.scopes.file(stub.source)?;
        let package = path(headers, scope.package, ".");
        let name = headers.lookup_names.get(stub.lookup_name?)?.to_string();
        let declaration = headers.syntax.declaration(stub.id);
        let (kind, has_receiver, arity) = match (stub.kind, declaration.map(|value| value.kind)) {
            (
                DeclarationKind::Function,
                Some(HeaderDeclarationKind::Callable {
                    receiver,
                    parameters,
                    ..
                }),
            ) => (
                0,
                receiver.is_some(),
                headers.syntax.parameters(parameters).len(),
            ),
            (DeclarationKind::Classifier, Some(HeaderDeclarationKind::Classifier { .. }))
            | (DeclarationKind::TypeAlias, Some(HeaderDeclarationKind::TypeAlias { .. })) => {
                (1, false, 0)
            }
            (DeclarationKind::Property, Some(HeaderDeclarationKind::Property { receiver, .. })) => {
                (2, receiver.is_some(), 0)
            }
            (DeclarationKind::Constructor, _)
            | (DeclarationKind::Accessor, _)
            | (DeclarationKind::Initializer, _)
            | (DeclarationKind::EnumEntry, _)
            | (DeclarationKind::Script, _)
            | (DeclarationKind::Function, _)
            | (DeclarationKind::Property, _)
            | (DeclarationKind::Classifier, _)
            | (DeclarationKind::TypeAlias, _) => return None,
        };
        Some((package, kind, name, has_receiver, arity))
    }

    let mut actuals = std::collections::HashMap::<Key, Vec<DeclarationId>>::new();
    for stub in headers
        .stubs
        .iter()
        .filter(|stub| !stub.flags.has(DeclarationFlags::EXPECT))
    {
        if let Some(key) = key(headers, stub) {
            actuals.entry(key).or_default().push(stub.id);
        }
    }
    let scopes = ClassifierScopes::of(headers);
    let mut incompatible = std::collections::HashSet::new();
    let mut select_top_level = |stub: &DeclarationStub, aliases: &[ActualizedAlias]| {
        let candidates = actuals.get(&key(headers, stub)?)?;
        let actual = select_actual(headers, stub, candidates, aliases, &scopes);
        if actual.is_none() {
            incompatible.insert(stub.id);
        }
        actual.map(|actual| ActualizedDeclarationPair {
            expect: stub.id,
            actual,
        })
    };
    let mut pairs = headers
        .stubs
        .iter()
        .filter(|stub| {
            stub.flags.has(DeclarationFlags::EXPECT) && stub.kind == DeclarationKind::Classifier
        })
        .filter_map(|stub| select_top_level(stub, &[]))
        .collect::<Vec<_>>();
    let actualized_aliases = pairs
        .iter()
        .filter_map(|pair| {
            let expect = headers.stubs.iter().find(|stub| stub.id == pair.expect)?;
            let spelling = headers.lookup_names.get(expect.lookup_name?)?;
            // The classifier the alias answers for, by identity: `Carried` in `plib` and `Carried`
            // in another package are two classifiers, and only one of them has been actualized.
            let package = headers
                .sources
                .get(expect.source)
                .map(|source| source.package.render().replace('/', "."))
                .unwrap_or_default();
            let name = if package.is_empty() {
                spelling.replace('$', ".")
            } else {
                format!("{package}.{}", spelling.replace('$', "."))
            };
            let HeaderDeclarationKind::TypeAlias { target, .. } =
                headers.syntax.declaration(pair.actual)?.kind
            else {
                return None;
            };
            let source = headers.declarations.anchor(pair.actual)?.source;
            Some((name, target, source))
        })
        .collect::<Vec<_>>();
    pairs.extend(
        headers
            .stubs
            .iter()
            .filter(|stub| {
                stub.flags.has(DeclarationFlags::EXPECT) && stub.kind != DeclarationKind::Classifier
            })
            .filter_map(|stub| select_top_level(stub, &actualized_aliases)),
    );

    fn child_key(
        headers: &StreamedHeaderModule,
        stub: &DeclarationStub,
        scopes: &ClassifierScopes,
        actualized_aliases: &[ActualizedAlias],
    ) -> Option<(DeclarationKind, String, String, usize)> {
        let name = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
            .unwrap_or_default()
            .to_string();
        // The receiver a member extension declares, as the CLASSIFIER it identifies. The two sides
        // of a pair may write it differently — one imports it, the other qualifies it, and an
        // `expect class` actualized by a `typealias` is written as the alias's target on the
        // platform side — so the key is the identity, after following an actualized alias to the
        // classifier it now stands for.
        let receiver_identity = |receiver: Option<HeaderTypeId>| {
            let Some(ty) = receiver.and_then(|ty| headers.syntax.ty(ty)) else {
                return String::new();
            };
            let HeaderTypeKind::Classifier { detail, .. } = ty.kind else {
                // A function-type receiver names no classifier; its rendered spelling is the whole
                // of what the coarse key can say about it, and `select_actual` compares the type
                // itself afterwards.
                return receiver
                    .and_then(|ty| headers.syntax.transient_type_ref(ty, &headers.lookup_names))
                    .map(|ty| ty.name)
                    .unwrap_or_default();
            };
            let Some(detail) = headers.syntax.classifier_type(detail) else {
                return String::new();
            };
            let written = headers
                .syntax
                .type_path(detail.path)
                .iter()
                .filter_map(|segment| headers.lookup_names.get(*segment))
                .collect::<Vec<_>>();
            let identity = scopes.resolve(stub.source, &written);
            for (name, target, source) in actualized_aliases {
                if *name != identity {
                    continue;
                }
                let Some(target) = headers.syntax.ty(*target) else {
                    continue;
                };
                let HeaderTypeKind::Classifier { detail, .. } = target.kind else {
                    continue;
                };
                let Some(detail) = headers.syntax.classifier_type(detail) else {
                    continue;
                };
                let written = headers
                    .syntax
                    .type_path(detail.path)
                    .iter()
                    .filter_map(|segment| headers.lookup_names.get(*segment))
                    .collect::<Vec<_>>();
                return scopes.resolve(*source, &written);
            }
            identity
        };
        let declaration = headers.syntax.declaration(stub.id);
        let (receiver, arity) = match declaration.map(|value| value.kind) {
            Some(HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                ..
            }) => (
                receiver_identity(receiver),
                headers.syntax.parameters(parameters).len(),
            ),
            Some(HeaderDeclarationKind::Constructor { parameters, .. }) => {
                (String::new(), headers.syntax.parameters(parameters).len())
            }
            Some(HeaderDeclarationKind::Property { receiver, .. }) => {
                (receiver_identity(receiver), 0)
            }
            Some(HeaderDeclarationKind::Classifier { .. })
            | Some(HeaderDeclarationKind::TypeAlias { .. })
            | None => (String::new(), 0),
        };
        Some((stub.kind, name, receiver, arity))
    }

    let mut next = 0;
    while next < pairs.len() {
        let pair = pairs[next];
        next += 1;
        let expect_children = headers.stubs.iter().filter(|stub| {
            headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner == Some(pair.expect))
        });
        let mut actual_children = std::collections::HashMap::<
            (DeclarationKind, String, String, usize),
            Vec<DeclarationId>,
        >::new();
        for child in headers.stubs.iter().filter(|stub| {
            headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner == Some(pair.actual))
        }) {
            if let Some(key) = child_key(headers, child, &scopes, &actualized_aliases) {
                actual_children.entry(key).or_default().push(child.id);
            }
        }
        for child in expect_children {
            let Some(key) = child_key(headers, child, &scopes, &actualized_aliases) else {
                continue;
            };
            let Some(candidates) = actual_children.get(&key) else {
                continue;
            };
            // The child key is deliberately coarse — kind, name, receiver and arity — so members
            // that differ only in a parameter's TYPE tie on it. `select_actual` is the same
            // comparison the top-level matcher already makes, and it is what tells
            // `actual fun foo(a: String)` from `actual fun foo(a: Any)` beside it. Giving up on a
            // tie leaves BOTH members unpaired, which reads downstream as two members that
            // actualized nothing.
            let Some(actual) =
                select_actual(headers, child, candidates, &actualized_aliases, &scopes)
            else {
                continue;
            };
            let pair = ActualizedDeclarationPair {
                expect: child.id,
                actual,
            };
            if !pairs.contains(&pair) {
                pairs.push(pair);
            }
        }
    }
    // Only a TOP-LEVEL implementation can be reported this way: the `actual` a member writes is
    // recorded on the member itself, which compact headers do not carry a flag for, so a member
    // pair says nothing about its modifier here.
    let unmarked = pairs
        .iter()
        .filter(|pair| {
            headers
                .declarations
                .anchor(pair.actual)
                .is_some_and(|anchor| anchor.owner.is_none())
                && headers
                    .stubs
                    .iter()
                    .find(|stub| stub.id == pair.actual)
                    .is_some_and(|stub| !stub.flags.has(DeclarationFlags::ACTUAL))
        })
        .map(|pair| pair.actual)
        .collect::<Vec<_>>();
    Actualization {
        pairs,
        unmarked,
        incompatible,
    }
}
