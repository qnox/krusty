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
    /// The declared result of a function, or the type of a property.
    pub(crate) result: Ty,
    /// A property declared `var`; always false for a function.
    pub(crate) mutable: bool,
    /// A function declared `operator`, `infix` or `inline`. kotlinc requires an implementation to
    /// state each of these its `expect` member states, and lets it state more.
    pub(crate) operator: bool,
    pub(crate) infix: bool,
    pub(crate) inline: bool,
}

/// What the inherited members of one name prove about an `expect` member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Discharge {
    /// Exactly one inherited member implements it.
    Implemented,
    /// No inherited member implements it, and every one was compared in full.
    Owed,
    /// No inherited member implements it, but one of them states a shape the record cannot
    /// compare (a type with arguments or a type parameter, an inferred result, a modifier the
    /// record does not carry, a `vararg`). That is not evidence the member is missing.
    Unprovable,
}

/// How the inherited members, of the one name `name`, answer for `expect`.
///
/// An implementation needs the same kind and name, no receiver and no context parameters on either
/// side, value parameters, result and (for a property) mutability that agree, and nothing else for
/// it to be confused with: the member is accepted only when exactly one inherited member
/// qualifies. A type that names a type parameter or carries type arguments has no classifier
/// identity to compare, so a candidate that needs one neither implements nor disproves the member.
pub(super) fn implementations<'a>(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    expect: &DeclarationStub,
    name: &str,
    members: impl Iterator<Item = &'a InheritedDependencyMember>,
) -> Discharge {
    let mut implementing = 0;
    let mut unprovable = false;
    for member in members {
        match implements(headers, bindings, actualized_aliases, expect, name, member) {
            Some(true) => implementing += 1,
            Some(false) => {}
            None => unprovable = true,
        }
    }
    match (implementing, unprovable) {
        (1, _) => Discharge::Implemented,
        (0, true) => Discharge::Unprovable,
        _ => Discharge::Owed,
    }
}

/// Whether `member` implements `expect`: `None` when the two have the same kind and name but a
/// shape the record cannot compare.
fn implements(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    actualized_aliases: &[(crate::types::TypeName, HeaderTypeId, SourceFileId)],
    expect: &DeclarationStub,
    name: &str,
    member: &InheritedDependencyMember,
) -> Option<bool> {
    // A constructor is named by its class, not by a member name the record keeps.
    let named_alike = expect.kind == DeclarationKind::Constructor || member.name == name;
    if member.kind != expect.kind || !named_alike {
        return Some(false);
    }
    if carries_unprojected_modifier(expect) {
        return None;
    }
    if !states_expected_modifiers(expect, member) {
        return Some(false);
    }
    let types = |written: HeaderTypeId, inherited: Ty| {
        type_matches(
            headers,
            bindings,
            actualized_aliases,
            expect.source,
            written,
            inherited,
        )
    };
    let all = |answers: &mut dyn Iterator<Item = Option<bool>>| {
        let mut unprovable = false;
        for answer in answers {
            match answer {
                Some(false) => return Some(false),
                None => unprovable = true,
                Some(true) => {}
            }
        }
        (!unprovable).then_some(true)
    };
    let parameters_match = |parameters: &[crate::fir::HeaderParameter]| {
        if parameters.len() != member.parameters.len() {
            return Some(false);
        }
        all(&mut parameters
            .iter()
            .zip(&member.parameters)
            .map(|(parameter, inherited)| {
                if parameter.flags.is_vararg() {
                    None
                } else {
                    types(parameter.ty, *inherited)
                }
            }))
    };
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
            if !headers.syntax.type_parameters(type_parameters).is_empty() {
                return None;
            }
            if member.mutable {
                return Some(false);
            }
            let result = match result {
                HeaderResultType::Explicit(written) => types(written, member.result),
                HeaderResultType::ImplicitUnit => Some(member.result.kind() == TyKind::Unit),
                // An inferred result states no type to compare.
                HeaderResultType::Inferred => None,
            };
            all(&mut [
                parameters_match(headers.syntax.parameters(parameters)),
                result,
            ]
            .into_iter())
        }
        Some(HeaderDeclarationKind::Constructor {
            context_parameters,
            parameters,
        }) => {
            if !headers.syntax.parameters(context_parameters).is_empty() {
                return None;
            }
            parameters_match(headers.syntax.parameters(parameters))
        }
        Some(HeaderDeclarationKind::Property {
            receiver: None,
            context_parameters,
            declared_type: Some(written),
            type_parameters,
            mutable,
            ..
        }) => {
            if !headers.syntax.parameters(context_parameters).is_empty()
                || !headers.syntax.type_parameters(type_parameters).is_empty()
            {
                return None;
            }
            if !member.parameters.is_empty() || mutable != member.mutable {
                return Some(false);
            }
            types(written, member.result)
        }
        _ => None,
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
/// type arguments, as `inherited`: `None` when either side has a shape this comparison does not
/// state (type arguments, a type parameter, a definitely-non-null type, an unbound classifier). A
/// Java platform type `T!` agrees with either nullability. An `expect` classifier an `actual
/// typealias` actualizes is the alias's target.
fn type_matches(
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
    let (inherited, nullability_agrees) = match inherited.kind() {
        TyKind::PlatformNullable(inner) => (inner, true),
        _ => (inherited, ty.flags.nullable() == inherited.is_nullable()),
    };
    match inherited.non_null().kind() {
        TyKind::Unit => Some(nullability_agrees && classifier == crate::types::KOTLIN_UNIT),
        TyKind::Obj(inherited, arguments) if arguments.is_empty() => {
            Some(nullability_agrees && classifier == inherited)
        }
        _ => None,
    }
}
