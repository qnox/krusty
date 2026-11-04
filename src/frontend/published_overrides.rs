//! The override relations a source-set analysis publishes for its classifiers' members, by stable
//! declaration identity, for a consumer that reads a source set's declarations without compiling it
//! (a KSP round).

use super::{FrontendDeclarationId, SourceSetAnalysis};
use crate::fir::{ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget};
use crate::types::TypeName;

/// A member that a source member directly overrides.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OverriddenMember {
    /// A member this module declares.
    Declared(FrontendDeclarationId),
    /// A member the library classifier `owner` declares, named `name`.
    Library { owner: TypeName, name: Box<str> },
}

impl SourceSetAnalysis {
    /// The classifier that declares the member `declaration`, or `None` when `declaration` is no
    /// member this analysis published.
    pub fn member_owner(
        &self,
        declaration: FrontendDeclarationId,
    ) -> Option<FrontendDeclarationId> {
        self.streamed
            .as_ref()?
            .module
            .index()
            .declaration_anchor(declaration)?
            .owner
    }

    /// The members that the member function or property `declaration` of the classifier `owner`
    /// directly overrides, as the frontend selected them.
    pub fn overridden_members(
        &self,
        owner: FrontendDeclarationId,
        declaration: FrontendDeclarationId,
    ) -> Vec<OverriddenMember> {
        let Some(streamed) = &self.streamed else {
            return Vec::new();
        };
        let index = streamed.module.index();
        let function = |target: ResolvedFunctionOverrideTarget| match target {
            ResolvedFunctionOverrideTarget::Module(callable) => {
                index.callable(callable).map(|header| header.declaration)
            }
            ResolvedFunctionOverrideTarget::External(_) => None,
        };
        let property = |target: ResolvedPropertyOverrideTarget| match target {
            ResolvedPropertyOverrideTarget::Module(property) => {
                index.property(property).map(|header| header.declaration)
            }
            ResolvedPropertyOverrideTarget::External(_) => None,
        };
        let overridden = |declared: Option<FrontendDeclarationId>, owner: TypeName, name: &str| {
            declared.map_or_else(
                || OverriddenMember::Library {
                    owner,
                    name: name.into(),
                },
                OverriddenMember::Declared,
            )
        };
        let functions = index
            .function_overrides(owner)
            .iter()
            .filter(|edge| function(edge.implementation) == Some(declaration))
            .map(|edge| overridden(function(edge.overridden), edge.overridden_owner, &edge.name));
        let properties = index
            .property_overrides(owner)
            .iter()
            .filter(|edge| property(edge.implementation) == Some(declaration))
            .map(|edge| overridden(property(edge.overridden), edge.overridden_owner, &edge.name));
        functions.chain(properties).collect()
    }
}
