//! Which `expect` headers this module's `actual` declarations answer for.
//!
//! The match decides which declarations survive into signature collection, so it consumes compact
//! Pass-1 syntax plus classifier identities bound beforehand by the ordinary resolver. It never
//! interprets a spelling itself: classifiers compare as qualified identities and type parameters by
//! position.

use super::*;

/// Match top-level multiplatform headers using compact Pass-1 facts and resolver-published
/// classifier identities. The returned stable ids identify expect declarations shadowed by a
/// same-package, same-shape non-expect declaration or type alias. No parser declaration id
/// participates in the match.
pub fn matched_expect_declarations(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
) -> std::collections::HashSet<DeclarationId> {
    actualized_declaration_pairs(headers, bindings)
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

/// Classifier identities already bound by the ordinary resolver for compact actualization syntax.
///
/// Actualization consumes these identities; it does not interpret packages, imports, default
/// imports, dependency providers, or unresolved spellings itself. An absent entry is unresolved or
/// ambiguous and therefore cannot participate in a declaration match.
#[derive(Default)]
pub struct ActualizationTypeBindings {
    by_type: std::collections::HashMap<(SourceFileId, HeaderTypeId), crate::types::TypeName>,
    by_declaration: std::collections::HashMap<DeclarationId, crate::types::TypeName>,
}

impl ActualizationTypeBindings {
    pub(crate) fn bind_type(
        &mut self,
        source: SourceFileId,
        syntax: HeaderTypeId,
        classifier: crate::types::TypeName,
    ) {
        self.by_type.insert((source, syntax), classifier);
    }

    pub(crate) fn bind_declaration(
        &mut self,
        declaration: DeclarationId,
        classifier: crate::types::TypeName,
    ) {
        self.by_declaration.insert(declaration, classifier);
    }

    fn type_classifier(
        &self,
        source: SourceFileId,
        syntax: HeaderTypeId,
    ) -> Option<crate::types::TypeName> {
        self.by_type.get(&(source, syntax)).copied()
    }

    fn declaration_classifier(&self, declaration: DeclarationId) -> Option<crate::types::TypeName> {
        self.by_declaration.get(&declaration).copied()
    }
}

/// [`Actualization::pairs`] alone, for callers that need no more.
pub fn actualized_declaration_pairs(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
) -> Vec<ActualizedDeclarationPair> {
    actualization(headers, bindings).pairs
}

/// Match actualized declaration subtrees from compact Pass-1 headers and already-bound classifier
/// identities. This is also the authority for expect-owned default expressions: callers publish
/// their presence on `actual`, but retain `expect` as the stable provider identity for Pass-2
/// checking.
pub fn actualization(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
) -> Actualization {
    type Key = (String, u8, String, bool, usize);
    /// One `expect` classifier actualized by a `typealias`: the classifier's QUALIFIED identity,
    /// the alias's target type, and the file the target is written in — which is the scope that
    /// target's own spelling resolves in.
    type ActualizedAlias = (crate::types::TypeName, HeaderTypeId, SourceFileId);

    /// The two files one comparison reads its spellings in, and the module scope both resolve
    /// against.
    #[derive(Clone, Copy)]
    struct MatchScope<'a> {
        bindings: &'a ActualizationTypeBindings,
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
        let expect_id = expect;
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
                            // Resolution bound each syntax node once through the ordinary scope
                            // tower. An unresolved or ambiguous node has no identity and cannot
                            // match, even when the other side has the same spelling.
                            match (
                                scope.bindings.type_classifier(scope.expect, expect_id),
                                scope
                                    .bindings
                                    .type_classifier(scope.candidate, candidate_id),
                            ) {
                                (Some(expect), Some(candidate)) => expect == candidate,
                                _ => false,
                            }
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
        if matches!(expect.kind, HeaderTypeKind::Classifier { .. }) {
            if let Some(identity) = scope.bindings.type_classifier(scope.expect, expect_id) {
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
        bindings: &ActualizationTypeBindings,
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
            bindings,
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
        bindings: &ActualizationTypeBindings,
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
            bindings,
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
        bindings: &ActualizationTypeBindings,
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
            bindings,
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
        bindings: &ActualizationTypeBindings,
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
                    bindings,
                ),
                DeclarationKind::Constructor => constructor_parameter_shapes_match(
                    headers,
                    expect.id,
                    *candidate,
                    actualized_aliases,
                    bindings,
                ),
                DeclarationKind::Property => property_input_shapes_match(
                    headers,
                    expect.id,
                    *candidate,
                    actualized_aliases,
                    bindings,
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
    let mut incompatible = std::collections::HashSet::new();
    let mut select_top_level = |stub: &DeclarationStub, aliases: &[ActualizedAlias]| {
        let candidates = actuals.get(&key(headers, stub)?)?;
        let actual = select_actual(headers, stub, candidates, aliases, bindings);
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
            // The classifier the alias answers for, by identity: `Carried` in `plib` and `Carried`
            // in another package are two classifiers, and only one of them has been actualized.
            let name = bindings.declaration_classifier(pair.expect)?;
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

    /// What a member extension's RECEIVER contributes to the coarse child key.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    enum ReceiverKey {
        /// The member declares no receiver.
        Absent,
        /// The classifier the receiver names, after following an actualized alias to whatever it
        /// now stands for.
        Classifier(crate::types::TypeName),
        /// A receiver that names no classifier — a function type. The coarse key records only the
        /// semantic category; `select_actual` compares its complete type shape.
        Structural,
        /// A receiver written as a classifier path that the scope binds to no classifier: a type
        /// PARAMETER, whose identity is positional within the declaration rather than a classifier
        /// at all — `actual val <S> S.p: S` declares its own `S`, shadowing its owner's.
        ///
        /// Refusing to key such a member at all left `expect val <S> S.p: S` and the `actual`
        /// written exactly like it pairing with nothing, and the implementation reported as
        /// actualizing nothing. The coarse key records the category, and `select_actual` compares
        /// the complete type shape — which is what tells two of them apart, positionally.
        Unbound,
    }

    fn child_key(
        headers: &StreamedHeaderModule,
        stub: &DeclarationStub,
        bindings: &ActualizationTypeBindings,
        actualized_aliases: &[ActualizedAlias],
    ) -> Option<(DeclarationKind, String, ReceiverKey, usize)> {
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
        // `None` means the file has not said which classifier the receiver is, so the member
        // cannot be keyed at all and pairs with nothing.
        let receiver_identity = |receiver: Option<HeaderTypeId>| -> Option<ReceiverKey> {
            let Some(receiver) = receiver else {
                return Some(ReceiverKey::Absent);
            };
            let Some(ty) = headers.syntax.ty(receiver) else {
                return None;
            };
            if !matches!(ty.kind, HeaderTypeKind::Classifier { .. }) {
                return Some(ReceiverKey::Structural);
            };
            let Some(identity) = bindings.type_classifier(stub.source, receiver) else {
                return Some(ReceiverKey::Unbound);
            };
            for (name, target, source) in actualized_aliases {
                if *name != identity {
                    continue;
                }
                return bindings
                    .type_classifier(*source, *target)
                    .map(ReceiverKey::Classifier);
            }
            Some(ReceiverKey::Classifier(identity))
        };
        let declaration = headers.syntax.declaration(stub.id);
        let (receiver, arity) = match declaration.map(|value| value.kind) {
            Some(HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                ..
            }) => (
                receiver_identity(receiver)?,
                headers.syntax.parameters(parameters).len(),
            ),
            Some(HeaderDeclarationKind::Constructor { parameters, .. }) => (
                ReceiverKey::Absent,
                headers.syntax.parameters(parameters).len(),
            ),
            Some(HeaderDeclarationKind::Property { receiver, .. }) => {
                (receiver_identity(receiver)?, 0)
            }
            Some(HeaderDeclarationKind::Classifier { .. })
            | Some(HeaderDeclarationKind::TypeAlias { .. })
            | None => (ReceiverKey::Absent, 0),
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
            (DeclarationKind, String, ReceiverKey, usize),
            Vec<DeclarationId>,
        >::new();
        for child in headers.stubs.iter().filter(|stub| {
            headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner == Some(pair.actual))
        }) {
            if let Some(key) = child_key(headers, child, bindings, &actualized_aliases) {
                actual_children.entry(key).or_default().push(child.id);
            }
        }
        for child in expect_children {
            let Some(key) = child_key(headers, child, bindings, &actualized_aliases) else {
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
                select_actual(headers, child, candidates, &actualized_aliases, bindings)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::source::SourceInput;

    /// Build the compact inventory for a set of `(stem, text)` sources.
    fn headers(sources: &[(&str, &str)]) -> StreamedHeaderModule {
        let inputs = sources
            .iter()
            .map(|(stem, text)| SourceInput::kotlin(text).with_file_stem(stem))
            .collect::<Vec<_>>();
        let mut diagnostics = DiagSink::new();
        let files = sources
            .iter()
            .map(|(_, text)| {
                crate::frontend::parse_source_with_detected_features(text, &mut diagnostics)
            })
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        super::super::inventory_parsed_source_headers(&inputs, &files)
    }

    fn resolve(
        headers: &StreamedHeaderModule,
        source: SourceFileId,
        segments: &[&str],
    ) -> Option<crate::types::TypeName> {
        crate::resolve::resolve_actualization_classifier_for_test(
            headers,
            source,
            &segments.join("."),
        )
    }

    /// The scope a file writes a classifier in decides which classifier it is, and every form the
    /// parser records is read: an explicit import, an import ALIAS, the file's own package, a
    /// fully-qualified spelling, and a wildcard import.
    #[test]
    fn every_import_form_names_the_classifier_it_brings_into_scope() {
        let headers = headers(&[
            ("Model", "package plib.model\n\nclass Tally\n"),
            ("Own", "package plib\n\nclass Ledger\n"),
            (
                "Uses",
                "package plib\n\
                 \n\
                 import plib.model.Tally\n\
                 import plib.model.Tally as Renamed\n\
                 \n\
                 fun explicit(value: Tally) {}\n",
            ),
            (
                "Wildcard",
                "package plib\n\
                 \n\
                 import plib.model.*\n\
                 \n\
                 fun starred(value: Tally) {}\n",
            ),
        ]);
        let uses = SourceFileId::from_raw(2);
        let wildcard = SourceFileId::from_raw(3);
        let tally = crate::types::type_name("plib/model/Tally");
        assert_eq!(
            resolve(&headers, uses, &["Tally"]),
            Some(tally),
            "an explicit import names the classifier its path ends at"
        );
        assert_eq!(
            resolve(&headers, uses, &["Renamed"]),
            Some(tally),
            "an alias renames the classifier, not its identity"
        );
        assert_eq!(
            resolve(&headers, uses, &["plib", "model", "Tally"]),
            Some(tally),
            "a fully-qualified spelling names the same classifier"
        );
        assert_eq!(
            resolve(&headers, uses, &["Ledger"]),
            Some(crate::types::type_name("plib/Ledger")),
            "the file's own package supplies what it declares"
        );
        assert_eq!(
            resolve(&headers, wildcard, &["Tally"]),
            Some(tally),
            "a wildcard import supplies the classifier it brings into scope"
        );
    }

    /// TWO wildcard imports that could each supply one simple name say nothing about which
    /// classifier is meant, so the path names none and pairs with nothing. Answering with the
    /// first match would pair declarations that name DIFFERENT classifiers.
    #[test]
    fn a_simple_name_two_wildcards_could_supply_names_no_classifier() {
        let headers = headers(&[
            ("Left", "package plib.left\n\nclass Tally\n"),
            ("Right", "package plib.right\n\nclass Tally\n"),
            (
                "Both",
                "package plib\n\
                 \n\
                 import plib.left.*\n\
                 import plib.right.*\n\
                 \n\
                 fun takes(value: Tally) {}\n",
            ),
            (
                "One",
                "package plib\n\
                 \n\
                 import plib.left.*\n\
                 \n\
                 fun takes(value: Tally) {}\n",
            ),
        ]);
        assert_eq!(
            resolve(&headers, SourceFileId::from_raw(2), &["Tally"]),
            None,
            "two wildcards could each supply it, so the file has named no classifier"
        );
        assert_eq!(
            resolve(&headers, SourceFileId::from_raw(3), &["Tally"]),
            Some(crate::types::type_name("plib/left/Tally")),
            "one wildcard says exactly which, so it resolves"
        );
    }

    /// Two EXPLICIT imports claiming one simple name say nothing about which classifier is meant
    /// either — the same rule two wildcards take. Importing one classifier twice is a repetition
    /// rather than a choice, and so is wildcard-importing one package twice.
    #[test]
    fn a_name_two_imports_claim_names_no_classifier_and_a_repeat_is_not_a_choice() {
        let headers = headers(&[
            ("Left", "package plib.left\n\nclass Tally\n"),
            ("Right", "package plib.right\n\nclass Tally\n"),
            (
                "Conflicting",
                "package plib\n\
                 \n\
                 import plib.left.Tally\n\
                 import plib.right.Tally\n\
                 \n\
                 fun takes(value: Tally) {}\n",
            ),
            (
                "Repeated",
                "package plib\n\
                 \n\
                 import plib.left.Tally\n\
                 import plib.left.Tally\n\
                 \n\
                 fun takes(value: Tally) {}\n",
            ),
            (
                "RepeatedWildcard",
                "package plib\n\
                 \n\
                 import plib.left.*\n\
                 import plib.left.*\n\
                 \n\
                 fun takes(value: Tally) {}\n",
            ),
        ]);
        let left = crate::types::type_name("plib/left/Tally");
        assert_eq!(
            resolve(&headers, SourceFileId::from_raw(2), &["Tally"]),
            None,
            "two imports claim the name, so the file has named no classifier"
        );
        assert_eq!(
            resolve(&headers, SourceFileId::from_raw(3), &["Tally"]),
            Some(left),
            "the same import written twice still names one classifier"
        );
        assert_eq!(
            resolve(&headers, SourceFileId::from_raw(4), &["Tally"]),
            Some(left),
            "and so does the same wildcard written twice"
        );
    }

    /// Default imports bind through the ordinary provider-backed scope rather than by preserving
    /// an unresolved spelling.
    #[test]
    fn a_default_import_binds_to_its_qualified_identity() {
        let headers = headers(&[("Plain", "package plib\n\nfun takes(value: Int) {}\n")]);
        assert_eq!(
            resolve(&headers, SourceFileId::from_raw(0), &["Int"]),
            Some(crate::types::type_name("kotlin/Int")),
            "the common default import binds Int to its qualified builtin identity",
        );
    }
}
