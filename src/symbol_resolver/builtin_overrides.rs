//! The builtin classes' implementations of a supertype's member.
//!
//! A value of a builtin class read through one of the class's supertypes (`CharSequence`,
//! `Number`, `Comparable<Int>`) is an instance of that class, so a call of the supertype's member
//! on it runs the class's override: `String.length` for `CharSequence.length`, `Int.toLong` for
//! `Number.toLong`. A target that carries such a value in a representation of its own (a string, a
//! box) dispatches the call itself, and asks here which declaration each builtin class runs. The
//! member hierarchy's override edges answer it; the member's name only collects the candidates.

use crate::fir::ExternalCallableId;
use crate::libraries::LibraryCallable;
use crate::symbol_source::SymbolSource;
use crate::types::{OverriddenDeclaration, Ty, TypeName, REPRESENTED_BUILTINS};

use super::{
    declared_member_declarations, function_declaration_identity, member_declarations_in_hierarchy,
    property_declaration_identity, supertype_preorder,
};

/// Whether a member is a function or a property, which decides the declarations overriding it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemberKind {
    Function,
    Property,
}

/// One member of a classifier, as the override edges of the declarations below it name it.
pub(crate) struct BaseMember<'a> {
    pub(crate) name: &'a str,
    pub(crate) kind: MemberKind,
    pub(crate) identity: OverriddenDeclaration,
}

/// One classifier's declaration overriding a member: the function, or the property's getter.
pub(crate) struct ClassifierOverride {
    pub(crate) callable: LibraryCallable,
    /// The declaration's own parameter and result types, before use-site substitution.
    pub(crate) identity: OverriddenDeclaration,
}

/// The declaration the classifier of `receiver` overrides `base` with, its own or the nearest one
/// it inherits; `None` when no declaration below `base`'s owner overrides it, so `base`'s own
/// implementation runs.
pub(crate) fn classifier_override(
    source: &dyn SymbolSource,
    receiver: Ty,
    base: &BaseMember<'_>,
) -> Option<ClassifierOverride> {
    let (functions, properties) =
        member_declarations_in_hierarchy(source, receiver, base.name).into_parts();
    // The hierarchy view keeps the most derived declaration of a member family, and a class
    // declares at most one override of a member.
    match base.kind {
        MemberKind::Function => functions
            .overloads
            .into_iter()
            .find(|function| {
                function
                    .callable
                    .overridden_declarations
                    .contains(&base.identity)
            })
            .map(|function| ClassifierOverride {
                identity: function_declaration_identity(&function),
                callable: function.callable,
            }),
        MemberKind::Property => properties
            .overloads
            .into_iter()
            .find(|property| {
                property
                    .getter
                    .overridden_declarations
                    .contains(&base.identity)
            })
            .map(|property| ClassifierOverride {
                identity: property_declaration_identity(&property),
                callable: property.getter,
            }),
    }
}

/// What one builtin class runs for a call of a supertype's member.
pub(crate) struct BuiltinOverride {
    /// The builtin type whose classifier is a proper subtype of the member's.
    pub(crate) receiver: Ty,
    /// The classifier's declaration overriding the member; `None` when it inherits the member's
    /// own implementation.
    pub(crate) declaration: Option<ClassifierOverride>,
}

/// Each builtin class whose classifier is a proper subtype of `owner`, with its override of
/// `owner`'s member `name` that the provider published as `identity`: the function, or the
/// property whose getter it is. Empty when no builtin classifier is below `owner`, or `owner`
/// declares no such member.
pub(crate) fn builtin_overrides(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
    identity: ExternalCallableId,
) -> Vec<BuiltinOverride> {
    let subtypes = builtin_subtypes(source, owner);
    if subtypes.is_empty() {
        return Vec::new();
    }
    let Some(base) = declared_member(source, owner, name, identity) else {
        return Vec::new();
    };
    subtypes
        .into_iter()
        .map(|receiver| BuiltinOverride {
            receiver,
            declaration: classifier_override(source, receiver, &base),
        })
        .collect()
}

/// The representation overrides for one selected external declaration, in the handoff form
/// consumed by checked IR and backends.
pub(crate) fn builtin_implementation_facts(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &str,
    identity: ExternalCallableId,
) -> Box<[crate::fir::BuiltinImplementation]> {
    builtin_overrides(source, owner, name, identity)
        .into_iter()
        .map(|implementation| {
            let declaration = implementation.declaration.map(|declaration| {
                crate::fir::BuiltinOverrideDeclaration {
                    operation: declaration
                        .callable
                        .compiler_intrinsic
                        .or(declaration.callable.member_realization.compiler_intrinsic()),
                    params: declaration.identity.params,
                    ret: declaration.identity.ret,
                }
            });
            crate::fir::BuiltinImplementation {
                receiver: implementation.receiver,
                declaration,
            }
        })
        .collect()
}

/// Every builtin class whose classifier is a proper subtype of `owner`: the builtin values an
/// instance of `owner` can be, which a target carrying them in representations of its own must
/// answer a call of `owner`'s member or a type test against `owner` for.
pub(crate) fn builtin_subtypes(source: &dyn SymbolSource, owner: TypeName) -> Vec<Ty> {
    REPRESENTED_BUILTINS
        .into_iter()
        .filter(|builtin| {
            builtin.kotlin_class_internal().is_some_and(|classifier| {
                supertype_preorder(source, classifier)
                    .get(&owner)
                    .is_some_and(|&visit| visit > 0)
            })
        })
        .collect()
}

/// `owner`'s own member `name` the provider published as `identity`: a function, or the property
/// `identity` is the getter of.
fn declared_member<'a>(
    source: &dyn SymbolSource,
    owner: TypeName,
    name: &'a str,
    identity: ExternalCallableId,
) -> Option<BaseMember<'a>> {
    let (functions, properties) =
        declared_member_declarations(source, Ty::obj_name(owner), name).into_parts();
    let published = |callable: &LibraryCallable| callable.external_identity == Some(identity);
    if let Some(function) = functions
        .overloads
        .iter()
        .find(|function| published(&function.callable))
    {
        return Some(BaseMember {
            name,
            kind: MemberKind::Function,
            identity: function_declaration_identity(function),
        });
    }
    properties
        .overloads
        .iter()
        .find(|property| published(&property.getter))
        .map(|property| BaseMember {
            name,
            kind: MemberKind::Property,
            identity: property_declaration_identity(property),
        })
}

#[cfg(test)]
mod tests;
