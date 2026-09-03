use crate::types::{Ty, TypeVariance};

use super::{
    DeclarationId, DeclarationNameId, ResolvedModuleIndex, ResolvedTy, TypeParameterId,
    UnpublishableType,
};

/// One resolved upper bound of a declaration-owned type parameter. The classifier/interface
/// distinction is semantic metadata needed by generic-signature emitters; a backend must not
/// rediscover it from a physical owner name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTypeParameterBound {
    pub ty: ResolvedTy,
    pub is_interface: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedTypeParameterFlags(u8);

impl ResolvedTypeParameterFlags {
    const IN: u8 = 1 << 0;
    const OUT: u8 = 1 << 1;
    const NON_NULL: u8 = 1 << 2;
    const REIFIED: u8 = 1 << 3;

    pub const fn new(variance: TypeVariance, non_null: bool, reified: bool) -> Self {
        let mut bits = match variance {
            TypeVariance::Invariant => 0,
            TypeVariance::In => Self::IN,
            TypeVariance::Out => Self::OUT,
        };
        if non_null {
            bits |= Self::NON_NULL;
        }
        if reified {
            bits |= Self::REIFIED;
        }
        Self(bits)
    }

    pub const fn variance(self) -> TypeVariance {
        if self.0 & Self::IN != 0 {
            TypeVariance::In
        } else if self.0 & Self::OUT != 0 {
            TypeVariance::Out
        } else {
            TypeVariance::Invariant
        }
    }

    pub const fn is_non_null(self) -> bool {
        self.0 & Self::NON_NULL != 0
    }

    pub const fn is_reified(self) -> bool {
        self.0 & Self::REIFIED != 0
    }
}

/// Compact persistent declaration facts for one type parameter. `id` is its semantic identity;
/// `name` is the declared metadata/display name, while `semantic_name` is the resolver's
/// alpha-renamed variable used inside published [`Ty`] shapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTypeParameterHeader {
    pub id: TypeParameterId,
    name: DeclarationNameId,
    semantic_name: DeclarationNameId,
    pub flags: ResolvedTypeParameterFlags,
    pub bounds: Box<[ResolvedTypeParameterBound]>,
}

impl ResolvedModuleIndex {
    pub fn publish_type_parameter(
        &mut self,
        declaration: DeclarationId,
        ordinal: u32,
        name: &str,
        semantic_name: &str,
        flags: ResolvedTypeParameterFlags,
        bounds: impl IntoIterator<Item = (Ty, bool)>,
    ) -> Result<TypeParameterId, UnpublishableType> {
        let bounds = bounds
            .into_iter()
            .map(|(ty, is_interface)| {
                Ok(ResolvedTypeParameterBound {
                    ty: ResolvedTy::new(ty)?,
                    is_interface,
                })
            })
            .collect::<Result<Vec<_>, UnpublishableType>>()?
            .into_boxed_slice();
        assert!(
            !self.type_parameters.contains_key(&(declaration, ordinal)),
            "a declaration type parameter may be published only once"
        );
        let id = TypeParameterId::from_raw(super::header::next_id(
            self.type_parameter_headers.len(),
            "resolved type parameters",
        ));
        let name = self.intern_declaration_name(name);
        let semantic_name = self.intern_declaration_name(semantic_name);
        self.type_parameter_headers
            .push(ResolvedTypeParameterHeader {
                id,
                name,
                semantic_name,
                flags,
                bounds,
            });
        self.type_parameter_owners.push((declaration, ordinal));
        self.type_parameters.insert((declaration, ordinal), id);
        Ok(id)
    }

    pub fn type_parameter(
        &self,
        declaration: DeclarationId,
        ordinal: u32,
    ) -> Option<TypeParameterId> {
        self.type_parameters.get(&(declaration, ordinal)).copied()
    }

    pub fn type_parameter_header(
        &self,
        parameter: TypeParameterId,
    ) -> Option<&ResolvedTypeParameterHeader> {
        self.type_parameter_headers.get(parameter.raw() as usize)
    }

    pub fn type_parameter_name(&self, parameter: TypeParameterId) -> Option<&str> {
        let header = self.type_parameter_header(parameter)?;
        self.declaration_names
            .get(header.name.raw() as usize)
            .map(AsRef::as_ref)
    }

    pub fn type_parameter_semantic_name(&self, parameter: TypeParameterId) -> Option<&str> {
        let header = self.type_parameter_header(parameter)?;
        self.declaration_names
            .get(header.semantic_name.raw() as usize)
            .map(AsRef::as_ref)
    }

    pub(crate) fn type_parameter_by_semantic_name(
        &self,
        semantic_name: &str,
    ) -> Option<TypeParameterId> {
        self.type_parameter_headers.iter().find_map(|parameter| {
            (self.type_parameter_semantic_name(parameter.id) == Some(semantic_name))
                .then_some(parameter.id)
        })
    }

    pub(super) fn type_parameter_storage_payload_bytes(&self) -> usize {
        self.type_parameters.len()
            * (std::mem::size_of::<(DeclarationId, u32)>() + std::mem::size_of::<TypeParameterId>())
            + self.type_parameter_owners.len() * std::mem::size_of::<(DeclarationId, u32)>()
            + self.type_parameter_headers.len() * std::mem::size_of::<ResolvedTypeParameterHeader>()
            + self
                .type_parameter_headers
                .iter()
                .map(|parameter| {
                    parameter.bounds.len() * std::mem::size_of::<ResolvedTypeParameterBound>()
                })
                .sum::<usize>()
    }
}
