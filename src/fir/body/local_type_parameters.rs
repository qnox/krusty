//! The type parameters a body-local function declares.
//!
//! A local function is not a module declaration, so its type parameters have no published
//! [`crate::fir::TypeParameterId`]. The checker records them on the function's body instead:
//! lowering signs the lifted method with them, behind the type parameters the function captures.

use super::FirBody;
use crate::fir::ResolvedTypeParameterBound;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirLocalTypeParameter {
    /// The declared name, which the lifted method's signature spells.
    pub name: Box<str>,
    /// The resolver's identity for this parameter inside published types.
    pub semantic_name: Box<str>,
    /// The written upper bounds in source order; empty for the implicit `Any?`.
    pub bounds: Box<[ResolvedTypeParameterBound]>,
    pub reified: bool,
}

impl FirBody {
    /// Record a local function's own type parameters, in declaration order.
    pub fn set_type_parameters(&mut self, parameters: Vec<FirLocalTypeParameter>) {
        self.type_parameters = parameters.into_boxed_slice();
    }

    pub fn type_parameters(&self) -> &[FirLocalTypeParameter] {
        &self.type_parameters
    }

    pub(super) fn type_parameter_payload_bytes(&self) -> usize {
        self.type_parameters
            .iter()
            .map(|parameter| {
                std::mem::size_of::<FirLocalTypeParameter>()
                    + parameter.name.len()
                    + parameter.semantic_name.len()
                    + parameter.bounds.len() * std::mem::size_of::<ResolvedTypeParameterBound>()
            })
            .sum()
    }
}
