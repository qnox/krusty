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
    expect: &DeclarationStub,
    name: &str,
    members: impl Iterator<Item = &'a InheritedDependencyMember>,
) -> usize {
    members
        .filter(|member| implements(headers, bindings, expect, name, member))
        .count()
}

fn implements(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    expect: &DeclarationStub,
    name: &str,
    member: &InheritedDependencyMember,
) -> bool {
    // A constructor is named by its class, not by a member name the record keeps.
    let named_alike = expect.kind == DeclarationKind::Constructor || member.name == name;
    if member.kind != expect.kind || !named_alike || carries_unprojected_modifier(expect) {
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
                                expect.source,
                                parameter.ty,
                                *inherited,
                            )
                    })
                && match result {
                    HeaderResultType::Explicit(written) => {
                        type_matches(headers, bindings, expect.source, written, member.result)
                    }
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
                && type_matches(headers, bindings, expect.source, written, member.result)
        }
        _ => false,
    }
}

/// A declaration modifier the inherited-member record does not carry. The record cannot prove the
/// two sides agree on it, so an `expect` member that states one is never discharged by an
/// inherited member, whatever else matches; the dependency side refuses the same forms.
fn carries_unprojected_modifier(expect: &DeclarationStub) -> bool {
    [
        DeclarationFlags::SUSPEND,
        DeclarationFlags::INLINE,
        DeclarationFlags::EXTERNAL,
        DeclarationFlags::TAILREC,
        DeclarationFlags::OPERATOR,
        DeclarationFlags::INFIX,
        DeclarationFlags::LATEINIT,
        DeclarationFlags::CONST,
    ]
    .into_iter()
    .any(|flag| expect.flags.has(flag))
}

/// Whether the written `expect` type names the same classifier, with the same nullability and no
/// type arguments, as `inherited`.
fn type_matches(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    source: SourceFileId,
    expect: HeaderTypeId,
    inherited: Ty,
) -> bool {
    let Some(ty) = headers.syntax.ty(expect) else {
        return false;
    };
    let HeaderTypeKind::Classifier { detail, .. } = ty.kind else {
        return false;
    };
    let Some(detail) = headers.syntax.classifier_type(detail) else {
        return false;
    };
    if !headers.syntax.type_operands(detail.arguments).is_empty()
        || ty.flags.definitely_non_null()
        || ty.flags.nullable() != inherited.is_nullable()
    {
        return false;
    }
    let TyKind::Obj(classifier, arguments) = inherited.non_null().kind() else {
        return false;
    };
    arguments.is_empty() && bindings.type_classifier(source, expect) == Some(classifier)
}
