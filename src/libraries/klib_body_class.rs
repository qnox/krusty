//! Declaration-owned class evidence for authenticating serialized member headers.

use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::semantic::KotlinModality;
use crate::types::{Ty, TypeName, TypeVariance};

#[cfg(test)]
pub(crate) mod test_fixture;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KlibBodyClassParameters {
    pub(crate) formals: Box<[Ty]>,
    pub(crate) bounds: Box<[Vec<Ty>]>,
    pub(crate) variances: Box<[TypeVariance]>,
    pub(crate) own_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KlibBodyClassContract {
    pub(crate) module: String,
    pub(crate) signature: KlibPublicIdSignature,
    pub(crate) classifier: TypeName,
    pub(crate) parameters: KlibBodyClassParameters,
    pub(crate) kind: super::TypeKind,
    pub(crate) modality: KotlinModality,
    pub(crate) is_value: bool,
    pub(crate) is_inner: bool,
    pub(crate) is_expect: bool,
    /// Exact primary declaration, including a private primary that is not a public candidate.
    pub(crate) primary_constructor: Option<KlibPublicIdSignature>,
}

impl KlibBodyClassContract {
    /// Whether the common type model carries this class's instances as one of its builtin types,
    /// as it does an unsigned integer, rather than as an instance of a class.
    pub(crate) fn is_builtin_type(&self) -> bool {
        crate::types::builtin_semantic(self.classifier).is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KlibStoredField {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) final_: bool,
    pub(crate) private: bool,
}
