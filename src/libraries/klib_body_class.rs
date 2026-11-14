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
    pub(crate) fn identity(&self) -> super::KlibClassIdentity {
        super::KlibClassIdentity {
            module: self.module.clone(),
            signature: self.signature.clone(),
        }
    }
    /// Whether this frozen class contract names the supplied module-qualified owner.
    pub fn matches_owner(&self, module: &str, classifier: TypeName) -> bool {
        self.module == module && self.classifier == classifier
    }

    /// Whether the authenticated declaration is an object rather than a class or interface.
    pub fn is_object(&self) -> bool {
        self.kind == super::TypeKind::Object
    }

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
    /// class it materializes: a value class, or a class or object without type parameters that
    /// is not sealed. A builtin type of the common type model is neither, and neither is
    /// `kotlin.Any`, the root of every class, which each target lays out itself.
    pub(crate) fn emits_members(&self) -> bool {
        if self.is_builtin_type() || self.classifier == crate::types::wk::any() {
            return false;
        }
        self.is_value
            || (matches!(self.kind, super::TypeKind::Class | super::TypeKind::Object)
                && matches!(
                    self.modality,
                    KotlinModality::Final | KotlinModality::Open | KotlinModality::Abstract
                )
                && !self.is_inner
                && !self.is_expect
                && self.parameters.formals.is_empty())
    }

    /// This class applied to its declared formals. A non-generic class is the bare classifier.
    pub(crate) fn self_type(&self) -> Ty {
        Ty::obj_args_name(self.classifier, &self.parameters.formals)
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixture::generic_member_declaration;
    use crate::types::type_name;

    #[test]
    fn class_contract_matches_its_module_qualified_owner() {
        let (contract, identity) = generic_member_declaration();

        assert!(contract.matches_owner(&identity.module, type_name("sample/Owner")));
        assert!(!contract.matches_owner("other", type_name("sample/Owner")));
        assert!(!contract.matches_owner(&identity.module, type_name("sample/Other")));
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
