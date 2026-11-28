//! Members an actual classifier inherits from a dependency supertype.
//!
//! The header matcher pairs an `expect` member with a member of the actual classifier's whole
//! scope. Source supertypes contribute their headers to that scope; a supertype from a dependency
//! has none, so the resolver that binds the classifier identities also publishes the members the
//! classifier inherits from its dependency supertypes. Those are the members the shared member
//! hierarchy selects (inheritance, overriding and specialization decided once, by the same
//! traversal calls use), normalized to the evidence an `expect` declaration states: kind, name,
//! input types, result type and mutability. An `expect` member that no source member implements is
//! implemented by the one inherited dependency member that agrees on all of it, as kotlinc's fake
//! override (or a delegated member through `A by impl`) is.

use super::super::*;
use crate::types::{Ty, TyKind};

/// One member function or property the shared member hierarchy selects for a classifier's
/// dependency supertypes.
#[derive(Clone, Debug)]
pub struct InheritedDependencyMember {
    pub(crate) kind: DeclarationKind,
    pub(crate) name: String,
    /// The value parameters of a function; empty for a property.
    pub(crate) parameters: Box<[Ty]>,
    /// The names of those parameters, as the declaring class wrote them. Empty when the provider
    /// published none.
    pub(crate) parameter_names: Box<[String]>,
    /// The declared result of a function, or the type of a property.
    pub(crate) result: Ty,
    /// A property declared `var`; always false for a function.
    pub(crate) mutable: bool,
    /// A function declared `operator`, `infix` or `inline`. kotlinc requires an implementation to
    /// state each of these its `expect` member states, and lets it state more.
    pub(crate) operator: bool,
    pub(crate) infix: bool,
    pub(crate) inline: bool,
    /// The record states the member completely: no receiver, context parameter, type parameter,
    /// `vararg` or modifier it cannot carry. Only a plain member can implement an `expect` member,
    /// and only a plain one can be rendered as a candidate that did not.
    pub(crate) plain: bool,
}

/// Why a same-named member of the actual scope does not implement an `expect` member, as the
/// reference compiler groups the candidates it lists.
///
/// Only the reasons measured against it are named; any other disagreement is `Unmeasured`, which
/// the report refuses to render rather than guess a sentence for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncompatibilityReason {
    /// The same number of value parameters, at least one of a different type.
    ParameterTypes,
    ValueParameterCount,
    /// An `expect` function answered by a property.
    FunctionVersusProperty,
    Unmeasured,
}

/// A member of the actual scope that shares an `expect` member's name and implements it by none of
/// its facts.
#[derive(Clone, Debug)]
pub struct IncompatibleCandidate {
    pub reason: IncompatibilityReason,
    pub member: InheritedDependencyMember,
}

/// The members, in the order the hierarchy published them, that carry `expect`'s name but do not
/// implement it, each with why.
pub(super) fn incompatible_candidates<'a>(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    expect: &DeclarationStub,
    name: &str,
    members: impl Iterator<Item = &'a InheritedDependencyMember>,
) -> Vec<IncompatibleCandidate> {
    members
        .filter(|member| {
            let same_kind = if expect.kind == DeclarationKind::Constructor {
                member.kind == DeclarationKind::Constructor
            } else {
                member.kind != DeclarationKind::Constructor && member.name == name
            };
            same_kind && !implements(headers, bindings, actualized_aliases, expect, name, member)
        })
        .map(|member| IncompatibleCandidate {
            reason: reason(headers, bindings, actualized_aliases, expect, member),
            member: member.clone(),
        })
        .collect()
}

fn reason(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    expect: &DeclarationStub,
    member: &InheritedDependencyMember,
) -> IncompatibilityReason {
    use IncompatibilityReason::{
        FunctionVersusProperty, ParameterTypes, Unmeasured, ValueParameterCount,
    };
    let parameters = match headers
        .syntax
        .declaration(expect.id)
        .map(|value| value.kind)
    {
        Some(HeaderDeclarationKind::Callable {
            receiver: None,
            parameters,
            type_parameters,
            context_count: 0,
            ..
        }) if headers.syntax.type_parameters(type_parameters).is_empty() => {
            headers.syntax.parameters(parameters)
        }
        Some(HeaderDeclarationKind::Constructor {
            context_parameters,
            parameters,
        }) if headers.syntax.parameters(context_parameters).is_empty() => {
            headers.syntax.parameters(parameters)
        }
        _ => return Unmeasured,
    };
    if !member.plain || carries_unprojected_modifier(expect) {
        return Unmeasured;
    }
    match (expect.kind, member.kind) {
        (DeclarationKind::Function, DeclarationKind::Property) => FunctionVersusProperty,
        (DeclarationKind::Function, DeclarationKind::Function)
        | (DeclarationKind::Constructor, DeclarationKind::Constructor) => {
            if parameters.len() != member.parameters.len() {
                return ValueParameterCount;
            }
            let compared = parameters
                .iter()
                .zip(&member.parameters)
                .map(|(parameter, inherited)| {
                    if parameter.flags.is_vararg() {
                        return None;
                    }
                    compare_type(
                        headers,
                        bindings,
                        actualized_aliases,
                        expect.source,
                        parameter.ty,
                        *inherited,
                    )
                })
                .collect::<Option<Vec<_>>>();
            match compared {
                Some(compared) if compared.contains(&false) => ParameterTypes,
                _ => Unmeasured,
            }
        }
        _ => Unmeasured,
    }
}

/// The inherited members, of the one name `name`, that implement `expect`.
///
/// An implementation needs the same kind and name, no receiver and no context parameters on either
/// side, value parameters, result and (for a property) mutability that agree, and nothing else for
/// it to be confused with: the member is accepted only when exactly one inherited member
/// qualifies. A type that names a type parameter or carries type arguments has no classifier
/// identity to compare and implements nothing here.
pub(super) fn implementations<'a>(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    expect: &DeclarationStub,
    name: &str,
    members: impl Iterator<Item = &'a InheritedDependencyMember>,
) -> usize {
    members
        .filter(|member| implements(headers, bindings, actualized_aliases, expect, name, member))
        .count()
}

fn implements(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    expect: &DeclarationStub,
    name: &str,
    member: &InheritedDependencyMember,
) -> bool {
    // A constructor is named by its class, not by a member name the record keeps.
    let named_alike = expect.kind == DeclarationKind::Constructor || member.name == name;
    if !member.plain
        || member.kind != expect.kind
        || !named_alike
        || carries_unprojected_modifier(expect)
        || !states_expected_modifiers(expect, member)
    {
        return false;
    }
    match headers
        .syntax
        .declaration(expect.id)
        .map(|value| value.kind)
    {
        Some(HeaderDeclarationKind::Callable {
            receiver: None,
            parameters,
            result,
            type_parameters,
            context_count: 0,
            ..
        }) => {
            let parameters = headers.syntax.parameters(parameters);
            headers.syntax.type_parameters(type_parameters).is_empty()
                && !member.mutable
                && parameters.len() == member.parameters.len()
                && parameters
                    .iter()
                    .zip(&member.parameters)
                    .all(|(parameter, inherited)| {
                        !parameter.flags.is_vararg()
                            && type_matches(
                                headers,
                                bindings,
                                actualized_aliases,
                                expect.source,
                                parameter.ty,
                                *inherited,
                            )
                    })
                && match result {
                    HeaderResultType::Explicit(written) => type_matches(
                        headers,
                        bindings,
                        actualized_aliases,
                        expect.source,
                        written,
                        member.result,
                    ),
                    HeaderResultType::ImplicitUnit => member.result.kind() == TyKind::Unit,
                    // An inferred result states no type to compare.
                    HeaderResultType::Inferred => false,
                }
        }
        Some(HeaderDeclarationKind::Constructor {
            context_parameters,
            parameters,
        }) => {
            let parameters = headers.syntax.parameters(parameters);
            headers.syntax.parameters(context_parameters).is_empty()
                && parameters.len() == member.parameters.len()
                && parameters
                    .iter()
                    .zip(&member.parameters)
                    .all(|(parameter, inherited)| {
                        !parameter.flags.is_vararg()
                            && type_matches(
                                headers,
                                bindings,
                                actualized_aliases,
                                expect.source,
                                parameter.ty,
                                *inherited,
                            )
                    })
        }
        Some(HeaderDeclarationKind::Property {
            receiver: None,
            context_parameters,
            declared_type: Some(written),
            type_parameters,
            mutable,
            ..
        }) => {
            member.parameters.is_empty()
                && headers.syntax.parameters(context_parameters).is_empty()
                && headers.syntax.type_parameters(type_parameters).is_empty()
                && mutable == member.mutable
                && type_matches(
                    headers,
                    bindings,
                    actualized_aliases,
                    expect.source,
                    written,
                    member.result,
                )
        }
        _ => false,
    }
}

/// Whether `member` states every function modifier `expect` states that kotlinc requires the
/// implementation to state too (its `FunctionModifiersNotSubset` check). The rule is one-directional:
/// `Int.plus` is `operator`, and implements an `expect fun plus` that is not.
fn states_expected_modifiers(expect: &DeclarationStub, member: &InheritedDependencyMember) -> bool {
    [
        (DeclarationFlags::OPERATOR, member.operator),
        (DeclarationFlags::INFIX, member.infix),
        (DeclarationFlags::INLINE, member.inline),
    ]
    .into_iter()
    .all(|(flag, stated)| stated || !expect.flags.has(flag))
}

/// A declaration modifier the inherited-member record does not carry. The record cannot prove the
/// two sides agree on it, so an `expect` member that states one is never discharged by an
/// inherited member, whatever else matches; the dependency side refuses the same forms.
fn carries_unprojected_modifier(expect: &DeclarationStub) -> bool {
    [
        DeclarationFlags::SUSPEND,
        DeclarationFlags::EXTERNAL,
        DeclarationFlags::TAILREC,
        DeclarationFlags::LATEINIT,
        DeclarationFlags::CONST,
    ]
    .into_iter()
    .any(|flag| expect.flags.has(flag))
}

/// Whether the written `expect` type names the same classifier, with the same nullability and no
/// type arguments, as `inherited`. An `expect` classifier an `actual typealias` actualizes is the
/// alias's target.
fn type_matches(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    source: SourceFileId,
    expect: HeaderTypeId,
    inherited: Ty,
) -> bool {
    compare_type(
        headers,
        bindings,
        actualized_aliases,
        source,
        expect,
        inherited,
    ) == Some(true)
}

/// [`type_matches`], or `None` where the two types cannot be compared by classifier identity: a
/// written type with arguments or an unbound spelling, or an inherited one that is not a plain
/// class or builtin scalar.
fn compare_type(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    source: SourceFileId,
    expect: HeaderTypeId,
    inherited: Ty,
) -> Option<bool> {
    let ty = headers.syntax.ty(expect)?;
    let HeaderTypeKind::Classifier { detail, .. } = ty.kind else {
        return None;
    };
    let detail = headers.syntax.classifier_type(detail)?;
    if !headers.syntax.type_operands(detail.arguments).is_empty() || ty.flags.definitely_non_null()
    {
        return None;
    }
    let mut classifier = bindings.type_classifier(source, expect)?;
    let mut aliases = actualized_aliases
        .iter()
        .filter(|(name, _, _)| *name == classifier);
    if let (Some((_, target, alias_source)), None) = (aliases.next(), aliases.next()) {
        classifier = bindings.type_classifier(*alias_source, *target)?;
    }
    let plain = |ty: Ty| {
        ty.is_builtin_scalar()
            || matches!(ty.kind(), TyKind::Obj(_, arguments) if arguments.is_empty())
    };
    let expected = Ty::obj_name(classifier).canonical_semantic();
    let inherited_class = inherited.non_null().canonical_semantic();
    if !plain(expected) || !plain(inherited_class) {
        return None;
    }
    Some(ty.flags.nullable() == inherited.is_nullable() && expected == inherited_class)
}
