//! The generic contract of an active body-local member, published from its written formals and the
//! semantic identities its checked header bound. A member function and an extension property own
//! their type parameters the same way, so both publish them through this one transaction.

use crate::ast::TypeRef;
use crate::fir::{DeclarationId, ResolvedModuleIndex, ResolvedTypeParameterFlags};
use crate::fx_hash::HashSet;
use crate::resolve::TypeInfo;
use crate::types::{Ty, TypeVariance};

/// One formal as `publish_type_parameters_atomically` takes it: ordinal, source and semantic
/// names, flags, and its resolved bounds with whether each is an interface.
type PreparedTypeParameter = (u32, String, String, ResolvedTypeParameterFlags, Vec<(Ty, bool)>);

/// The type parameters a body-local member declares in source.
pub(super) struct WrittenTypeParameters<'a> {
    pub(super) names: &'a [String],
    pub(super) bounds: &'a [(String, TypeRef)],
    pub(super) reified: &'a HashSet<String>,
    /// Where the checked header recorded the formals' semantic identities.
    pub(super) declaration_start: u32,
}

impl WrittenTypeParameters<'_> {
    /// Publish the declaration's generic contract unless an earlier pass already did. Returns
    /// whether the contract is now published.
    pub(super) fn publish(
        &self,
        info: &TypeInfo,
        index: &mut ResolvedModuleIndex,
        declaration: DeclarationId,
    ) -> bool {
        let existing = (0..self.names.len())
            .filter(|&ordinal| index.type_parameter(declaration, ordinal as u32).is_some())
            .count();
        assert!(
            existing == 0 || existing == self.names.len(),
            "a declaration generic contract is published atomically"
        );
        if existing != 0 {
            return true;
        }
        let Some(parameters) = self.prepare(info) else {
            return false;
        };
        index
            .publish_type_parameters_atomically(declaration, parameters)
            .is_ok()
    }

    fn prepare(&self, info: &TypeInfo) -> Option<Vec<PreparedTypeParameter>> {
        let semantic_names = info.resolved_declaration_type_parameters(self.declaration_start);
        if semantic_names.len() != self.names.len() {
            return None;
        }
        self.names
            .iter()
            .zip(semantic_names)
            .enumerate()
            .map(|(ordinal, (source_name, semantic_name))| {
                let bounds = self
                    .bounds
                    .iter()
                    .filter(|(formal, _)| formal == source_name)
                    .map(|(_, bound)| info.resolved_type_bound(bound))
                    .collect::<Option<Vec<_>>>()?;
                Some((
                    ordinal as u32,
                    source_name.clone(),
                    semantic_name.clone(),
                    ResolvedTypeParameterFlags::new(
                        TypeVariance::Invariant,
                        false,
                        self.reified.contains(source_name),
                    ),
                    bounds,
                ))
            })
            .collect()
    }
}
