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

/// Classifier identity for compact headers.
///
/// Actualization runs before signatures resolve, so the identity available is the one each
/// file's package and import list establish over the classifiers the MODULE declares. That
/// list is the file scope the parser already published — `headers.scopes`, carrying the
/// package, every explicit import, every import ALIAS and every WILDCARD import — rather than
/// a second one rebuilt from the types a file happens to mention, which saw neither an alias
/// nor a wildcard and so read `import plib.model.Tally as Ledger` as naming `Ledger`.
///
/// The answer is a [`TypeName`]: the interned classifier identity the rest of the compiler
/// compares by, so a comparison is an id equality. A spelling is only ever an input to
/// interning here, never the thing compared.
///
/// Two limits are stated rather than hidden:
///
/// - A header phase cannot ask a PROVIDER what a path names — a dependency's classifiers are
///   not inventoried at this point. A path nothing in the module declares therefore resolves
///   to its own written form, which both sides of a comparison reach the same way (`Int`
///   against `Int` is one classifier however the module spells it). Two DIFFERENT dependency
///   classifiers written identically would pair here; signature checking rejects the pair
///   afterwards, which is where a provider view exists.
/// - A simple name more than one wildcard import could supply is AMBIGUOUS. The file has not
///   said which classifier it means, so it names none: `resolve` answers `None` and nothing
///   pairs on it. Answering with the first match would pair two declarations that name
///   different classifiers.
struct ClassifierScopes {
    /// Every classifier and type alias this module declares, by qualified identity.
    declared: std::collections::HashSet<crate::types::TypeName>,
    files: std::collections::HashMap<u32, FileScope>,
}

/// One file's resolution scope, as the parser recorded it.
struct FileScope {
    /// The package the file declares; `None` is the default package.
    package: Option<crate::types::TypeName>,
    /// The classifier an explicit import brings into scope, keyed by the simple name it is
    /// written as — the import's ALIAS where it wrote one, and the last segment of its path
    /// otherwise. `None` where more than one import claims that name: the file has then said
    /// nothing about which classifier it means, exactly as two wildcards say nothing.
    explicit: std::collections::HashMap<String, Option<String>>,
    /// Every wildcard import's package path, `/`-joined, in source order.
    wildcards: Vec<String>,
}

/// The qualified identity of a `/`-joined path under an optional package.
///
/// Every identity in this module is built through this one function, so a classifier
/// interned as a module declaration and the same classifier reached through an import or a
/// wildcard are the same `TypeName` rather than two that merely render alike.
fn qualified(package: Option<crate::types::TypeName>, path: &str) -> crate::types::TypeName {
    match package {
        Some(package) => crate::types::type_name(&format!("{}/{path}", package.render())),
        None => crate::types::type_name(path),
    }
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
            // A hoisted nested classifier publishes `Outer$Inner`; a use site writes
            // `Outer.Inner`. Both name one classifier, and the interner is told so.
            declared.insert(qualified(
                headers
                    .sources
                    .get(stub.source)
                    .map(|source| source.package)
                    .filter(|package| !package.render().is_empty()),
                &spelling.replace('$', "/"),
            ));
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
                .map(|entry| entry.package)
                .filter(|package| !package.render().is_empty());
            let mut explicit = std::collections::HashMap::new();
            let mut wildcards = Vec::new();
            if let Some(scope) = headers.scopes.file(source) {
                for import in headers.scopes.imports(scope.imports) {
                    let segments = headers
                        .scopes
                        .path(import.path)
                        .iter()
                        .filter_map(|segment| headers.lookup_names.get(*segment))
                        .collect::<Vec<_>>();
                    if segments.is_empty() {
                        continue;
                    }
                    if import.wildcard {
                        wildcards.push(segments.join("/"));
                        continue;
                    }
                    // The name the import brings into scope is what the file will WRITE: the
                    // alias when it renamed the import, and the path's last segment otherwise.
                    let written = import
                        .alias
                        .and_then(|alias| headers.lookup_names.get(alias))
                        .unwrap_or_else(|| segments[segments.len() - 1]);
                    let path = segments.join("/");
                    match explicit.entry(written.to_string()) {
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert(Some(path));
                        }
                        std::collections::hash_map::Entry::Occupied(mut slot) => {
                            // A second import under one name is a conflict unless both name the
                            // same classifier, which is a repetition rather than a choice.
                            if slot.get().as_deref() != Some(path.as_str()) {
                                slot.insert(None);
                            }
                        }
                    }
                }
            }
            files.insert(
                index as u32,
                FileScope {
                    package,
                    explicit,
                    wildcards,
                },
            );
        }
        Self { declared, files }
    }

    /// The classifier a written path names, as seen from `source`, or `None` where the file
    /// has not said which classifier it means.
    fn resolve(&self, source: SourceFileId, segments: &[&str]) -> Option<crate::types::TypeName> {
        let written = segments.join("/");
        let Some(scope) = self.files.get(&source.raw()) else {
            return Some(qualified(None, &written));
        };
        // An explicit import — aliased or not — names the classifier its path ends at, and
        // anything written under it hangs off that.
        if let Some(first) = segments.first() {
            if let Some(imported) = scope.explicit.get(*first) {
                // Two imports claiming one name leave the file naming no classifier under it.
                let imported = imported.as_ref()?;
                let rest = segments[1..].join("/");
                return Some(qualified(
                    None,
                    &if rest.is_empty() {
                        imported.clone()
                    } else {
                        format!("{imported}/{rest}")
                    },
                ));
            }
        }
        // The file's own package, then a fully-qualified spelling — both only when the module
        // declares such a classifier, since neither reading may invent one.
        let in_package = qualified(scope.package, &written);
        if self.declared.contains(&in_package) {
            return Some(in_package);
        }
        let as_written = qualified(None, &written);
        if self.declared.contains(&as_written) {
            return Some(as_written);
        }
        // A wildcard import. More than one DISTINCT classifier that could supply this name is
        // ambiguous; one package imported twice is a repetition, not a choice.
        let mut supplied: Vec<crate::types::TypeName> = Vec::new();
        for package in &scope.wildcards {
            let candidate = qualified(None, &format!("{package}/{written}"));
            if self.declared.contains(&candidate) && !supplied.contains(&candidate) {
                supplied.push(candidate);
            }
        }
        match supplied.as_slice() {
            [] => {}
            [single] => return Some(*single),
            _ => return None,
        }
        // Nothing the module declares or this file imports claims the path, so the path is its
        // own canonical form — reached identically from either side of a comparison.
        Some(as_written)
    }
}

/// Match actualized declaration subtrees using compact Pass-1 headers only. This is also the
/// authority for expect-owned default expressions: callers publish their presence on `actual`, but
/// retain `expect` as the stable provider identity for Pass-2 checking.
pub fn actualization(headers: &StreamedHeaderModule) -> Actualization {
    type Key = (String, u8, String, bool, usize);
    /// One `expect` classifier actualized by a `typealias`: the classifier's QUALIFIED identity,
    /// the alias's target type, and the file the target is written in — which is the scope that
    /// target's own spelling resolves in.
    type ActualizedAlias = (crate::types::TypeName, HeaderTypeId, SourceFileId);

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
                            // A path either side is ambiguous about names no classifier, so it
                            // matches nothing — not even another ambiguous one.
                            match (
                                scope.scopes.resolve(scope.expect, &segments(expect_path)),
                                scope
                                    .scopes
                                    .resolve(scope.candidate, &segments(candidate_path)),
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
        if let HeaderTypeKind::Classifier { detail, .. } = expect.kind {
            if let Some(detail) = headers.syntax.classifier_type(detail) {
                let written = headers
                    .syntax
                    .type_path(detail.path)
                    .iter()
                    .filter_map(|segment| headers.lookup_names.get(*segment))
                    .collect::<Vec<_>>();
                // An ambiguous path names no classifier, so it follows no alias either.
                let Some(identity) = scope.scopes.resolve(scope.expect, &written) else {
                    return false;
                };
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
            // Built the same way a declared classifier's identity is, so the two intern alike.
            let name = qualified(
                headers
                    .sources
                    .get(expect.source)
                    .map(|source| source.package)
                    .filter(|package| !package.render().is_empty()),
                &spelling.replace('$', "/"),
            );
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
        /// A receiver that names no classifier — a function type. Its rendered spelling is the
        /// whole of what a coarse key can say; `select_actual` compares the type itself.
        Structural(String),
    }

    fn child_key(
        headers: &StreamedHeaderModule,
        stub: &DeclarationStub,
        scopes: &ClassifierScopes,
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
            let Some(ty) = receiver.and_then(|ty| headers.syntax.ty(ty)) else {
                return Some(ReceiverKey::Absent);
            };
            let HeaderTypeKind::Classifier { detail, .. } = ty.kind else {
                // A function-type receiver names no classifier; its rendered spelling is the whole
                // of what the coarse key can say about it, and `select_actual` compares the type
                // itself afterwards.
                return Some(ReceiverKey::Structural(
                    receiver
                        .and_then(|ty| headers.syntax.transient_type_ref(ty, &headers.lookup_names))
                        .map(|ty| ty.name)
                        .unwrap_or_default(),
                ));
            };
            let Some(detail) = headers.syntax.classifier_type(detail) else {
                return Some(ReceiverKey::Absent);
            };
            let written = headers
                .syntax
                .type_path(detail.path)
                .iter()
                .filter_map(|segment| headers.lookup_names.get(*segment))
                .collect::<Vec<_>>();
            let identity = scopes.resolve(stub.source, &written)?;
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
                return scopes
                    .resolve(*source, &written)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::source::SourceInput;

    /// Build the compact inventory for a set of `(stem, text)` sources, and the scope resolver
    /// over it.
    fn scopes(sources: &[(&str, &str)]) -> (StreamedHeaderModule, ClassifierScopes) {
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
        let headers = super::super::inventory_parsed_source_headers(&inputs, &files);
        let scopes = ClassifierScopes::of(&headers);
        (headers, scopes)
    }

    /// The scope a file writes a classifier in decides which classifier it is, and every form the
    /// parser records is read: an explicit import, an import ALIAS, the file's own package, a
    /// fully-qualified spelling, and a wildcard import.
    #[test]
    fn every_import_form_names_the_classifier_it_brings_into_scope() {
        let (_, scopes) = scopes(&[
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
            scopes.resolve(uses, &["Tally"]),
            Some(tally),
            "an explicit import names the classifier its path ends at"
        );
        assert_eq!(
            scopes.resolve(uses, &["Renamed"]),
            Some(tally),
            "an alias renames the classifier, not its identity"
        );
        assert_eq!(
            scopes.resolve(uses, &["plib", "model", "Tally"]),
            Some(tally),
            "a fully-qualified spelling names the same classifier"
        );
        assert_eq!(
            scopes.resolve(uses, &["Ledger"]),
            Some(crate::types::type_name("plib/Ledger")),
            "the file's own package supplies what it declares"
        );
        assert_eq!(
            scopes.resolve(wildcard, &["Tally"]),
            Some(tally),
            "a wildcard import supplies the classifier it brings into scope"
        );
    }

    /// TWO wildcard imports that could each supply one simple name say nothing about which
    /// classifier is meant, so the path names none and pairs with nothing. Answering with the
    /// first match would pair declarations that name DIFFERENT classifiers.
    #[test]
    fn a_simple_name_two_wildcards_could_supply_names_no_classifier() {
        let (_, scopes) = scopes(&[
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
            scopes.resolve(SourceFileId::from_raw(2), &["Tally"]),
            None,
            "two wildcards could each supply it, so the file has named no classifier"
        );
        assert_eq!(
            scopes.resolve(SourceFileId::from_raw(3), &["Tally"]),
            Some(crate::types::type_name("plib/left/Tally")),
            "one wildcard says exactly which, so it resolves"
        );
    }

    /// Two EXPLICIT imports claiming one simple name say nothing about which classifier is meant
    /// either — the same rule two wildcards take. Importing one classifier twice is a repetition
    /// rather than a choice, and so is wildcard-importing one package twice.
    #[test]
    fn a_name_two_imports_claim_names_no_classifier_and_a_repeat_is_not_a_choice() {
        let (_, scopes) = scopes(&[
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
            scopes.resolve(SourceFileId::from_raw(2), &["Tally"]),
            None,
            "two imports claim the name, so the file has named no classifier"
        );
        assert_eq!(
            scopes.resolve(SourceFileId::from_raw(3), &["Tally"]),
            Some(left),
            "the same import written twice still names one classifier"
        );
        assert_eq!(
            scopes.resolve(SourceFileId::from_raw(4), &["Tally"]),
            Some(left),
            "and so does the same wildcard written twice"
        );
    }

    /// A path nothing the module declares or the file imports claims keeps its own written form.
    /// That is a canonical answer both sides of a comparison reach identically — `Int` against
    /// `Int` is one classifier however the module spells it — not a fallback to the text.
    #[test]
    fn an_unclaimed_path_is_its_own_canonical_form() {
        let (_, scopes) = scopes(&[("Plain", "package plib\n\nfun takes(value: Int) {}\n")]);
        assert_eq!(
            scopes.resolve(SourceFileId::from_raw(0), &["Int"]),
            Some(crate::types::type_name("Int")),
        );
    }
}
