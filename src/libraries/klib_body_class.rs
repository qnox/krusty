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
pub(crate) struct KlibBodyClassContract {
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

    /// Whether a support unit emits this class's members as functions of their receiver value,
    /// materializing no class: a final builtin type of the common type model that is not a value
    /// class, whose values the target carries itself. Such a member dispatches statically.
    pub(crate) fn emits_receiver_members(&self) -> bool {
        self.is_builtin_type()
            && !self.is_value
            && self.kind == super::TypeKind::Class
            && self.modality == KotlinModality::Final
            && self.parameters.formals.is_empty()
    }

    /// Whether a support unit materializes this class and emits its members as functions of the
    /// class it materializes: a value class, or a class without type parameters that is not
    /// sealed. A builtin type of the common type model is neither.
    pub(crate) fn emits_members(&self) -> bool {
        if self.is_builtin_type() {
            return false;
        }
        self.is_value
            || (self.kind == super::TypeKind::Class
                && matches!(
                    self.modality,
                    KotlinModality::Final | KotlinModality::Open | KotlinModality::Abstract
                )
                && !self.is_inner
                && !self.is_expect
                && self.parameters.formals.is_empty())
    }
}

/// A complete declared instance layout. Its field ordinals belong to this exact producer class;
/// retaining the layout grants no constructor, initializer, singleton or descriptor definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KlibClassStorage {
    pub(crate) declaration: KlibBodyClassContract,
    pub(crate) superclass: Ty,
    pub(crate) fields: Vec<KlibStoredField>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KlibStoredField {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) final_: bool,
    pub(crate) private: bool,
}
