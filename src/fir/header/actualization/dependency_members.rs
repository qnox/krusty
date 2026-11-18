//! Members an actual classifier inherits from a dependency supertype.
//!
//! The header matcher pairs an `expect` member with a member of the actual classifier's whole
//! scope. Source supertypes contribute their headers to that scope; a supertype from a dependency
//! has none, so the resolver that binds the classifier identities also publishes the members such
//! a supertype declares, as the provider normalized them. An `expect` member that no source member
//! implements is implemented by an inherited dependency member of the same kind, name and input
//! signature, as kotlinc's fake override (or a delegated member through `A by impl`) is.

use super::super::*;
use crate::types::{Ty, TyKind, TypeName};

/// One member function or property a dependency supertype declares, by its declared input shape.
#[derive(Clone, Debug)]
pub struct InheritedDependencyMember {
    pub(crate) kind: DeclarationKind,
    pub(crate) name: String,
    /// The value parameters of a function; empty for a property.
    pub(crate) parameters: Box<[Ty]>,
}

/// Whether `member` implements `expect`: the same kind and name, no receiver and no context
/// parameters on either side, and value parameters naming the same classifiers with the same
/// nullability. A parameter that names a type parameter or carries type arguments has no
/// classifier identity to compare and implements nothing here.
pub(super) fn implements(
    headers: &StreamedHeaderModule,
    bindings: &ActualizationTypeBindings,
    expect: &DeclarationStub,
    name: &str,
    member: &InheritedDependencyMember,
) -> bool {
    if member.kind != expect.kind || member.name != name {
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
            type_parameters,
            context_count: 0,
            ..
        }) => {
            let parameters = headers.syntax.parameters(parameters);
            headers.syntax.type_parameters(type_parameters).is_empty()
                && parameters.len() == member.parameters.len()
                && parameters
                    .iter()
                    .zip(&member.parameters)
                    .all(|(parameter, inherited)| {
                        !parameter.flags.is_vararg()
                            && parameter_matches(
                                headers,
                                bindings,
                                expect.source,
                                parameter.ty,
                                *inherited,
                            )
                    })
        }
        Some(HeaderDeclarationKind::Property { receiver: None, .. }) => {
            member.parameters.is_empty()
        }
        _ => false,
    }
}

fn parameter_matches(
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

/// The members `classifier`, a dependency classifier, and its supertypes declare, nearest first,
/// as `lookup` publishes them. Private members are never inherited.
pub(crate) fn inherited_members(
    classifier: TypeName,
    lookup: impl Fn(TypeName) -> Option<std::sync::Arc<crate::libraries::LibraryType>>,
) -> Vec<InheritedDependencyMember> {
    let mut members = Vec::new();
    let mut seen = vec![classifier];
    let mut next = 0;
    while let Some(&owner) = seen.get(next) {
        next += 1;
        let Some(declaration) = lookup(owner) else {
            continue;
        };
        for name in &declaration.declared_callable_order {
            let Some(callables) = declaration.declared_callables.get(name) else {
                continue;
            };
            members.extend(
                callables
                    .functions()
                    .iter()
                    .filter(|function| {
                        function.kind == crate::libraries::FnKind::Member
                            && function.visibility != crate::types::Visibility::Private
                    })
                    .map(|function| InheritedDependencyMember {
                        kind: DeclarationKind::Function,
                        name: name.clone(),
                        parameters: function.callable.params.clone().into_boxed_slice(),
                    }),
            );
            members.extend(
                callables
                    .properties()
                    .iter()
                    .filter(|property| {
                        property.kind == crate::libraries::PropKind::Member
                            && property.getter.visibility != crate::types::Visibility::Private
                    })
                    .map(|_| InheritedDependencyMember {
                        kind: DeclarationKind::Property,
                        name: name.clone(),
                        parameters: Box::default(),
                    }),
            );
        }
        for supertype in declaration.supertypes.iter() {
            if !seen.contains(&supertype) {
                seen.push(supertype);
            }
        }
    }
    members
}
