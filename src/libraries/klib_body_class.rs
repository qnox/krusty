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
    /// Whether the checked class belongs to this exact producer module and classifier.
    pub fn matches_owner(&self, module: &str, classifier: TypeName) -> bool {
        self.module == module && self.classifier == classifier
    }

    /// Whether this exact serialized ordinary class or object forbids subclass dispatch. Enum entries may
    /// override their enum's members, so an enum's final modality is not sufficient evidence.
    pub fn proves_final_class(&self, identity: &super::KlibClassIdentity, owner: TypeName) -> bool {
        self.matches_owner(&identity.module, owner)
            && self.signature == identity.signature
            && matches!(self.kind, super::TypeKind::Class | super::TypeKind::Object)
            && self.modality == KotlinModality::Final
            && !self.is_expect
    }

    /// Whether the common type model carries this class's instances as one of its builtin types,
    /// as it does an unsigned integer or an array, rather than as an instance of a class. An
    /// unsigned array is a value class over its signed array, not an array the target carries.
    pub(crate) fn is_builtin_type(&self) -> bool {
        crate::types::builtin_semantic(self.classifier).is_some()
            || (!self.is_value && Ty::obj_name(self.classifier).is_array())
    }

    /// Whether a support unit lays out a box of this value class, whose boxed instances dispatch
    /// to its overrides. A value class the type model classifies as a builtin scalar or array (an
    /// unsigned number or array) has none: its boxes are the target's.
    pub(crate) fn has_unit_box(&self) -> bool {
        let modeled = Ty::obj_name(self.classifier).canonical_semantic();
        self.is_value && !modeled.is_builtin_scalar() && !modeled.is_array()
    }

    /// Whether a support unit emits this class's members as functions of their receiver value,
    /// materializing no class: a final builtin type of the common type model that is not a value
    /// class, whose values the target carries itself, `Array<T>` included. Such a member
    /// dispatches statically.
    pub(crate) fn emits_receiver_members(&self) -> bool {
        self.is_builtin_type()
            && !self.is_value
            && self.kind == super::TypeKind::Class
            && self.modality == KotlinModality::Final
    }

    /// Whether a support unit emits this interface's default bodies as functions of the instance
    /// a class's dispatch record calls them on, materializing nothing: the interface lays out
    /// no storage, and its receiver is the implementing instance as the interface type. A call
    /// through the interface stays a virtual one; only a class that inherits the default
    /// reaches the body.
    pub(crate) fn emits_default_members(&self) -> bool {
        self.kind == super::TypeKind::Interface && !self.is_expect && !self.is_builtin_type()
    }

    /// Whether a support unit materializes this class and emits its members as functions of the
    /// class it materializes: a value class, or a class, object or enum class that is not sealed,
    /// inner or expected; a generic one carries its formals, which its members' bodies read. A
    /// builtin type of the common type model is neither, and neither is `kotlin.Any`, the root of
    /// every class, which each target lays out itself.
    pub(crate) fn emits_members(&self) -> bool {
        if self.is_builtin_type() || self.classifier == crate::types::wk::any() {
            return false;
        }
        self.is_value
            || (matches!(
                self.kind,
                super::TypeKind::Class | super::TypeKind::Object | super::TypeKind::Enum
            ) && matches!(
                self.modality,
                KotlinModality::Final | KotlinModality::Open | KotlinModality::Abstract
            ) && !self.is_inner
                && !self.is_expect)
    }

    /// Whether the class's own declaration creates every instance of it, through its private
    /// primary constructor: an object its one instance, an enum class each of its entries.
    pub(crate) fn declares_instances(&self) -> bool {
        matches!(self.kind, super::TypeKind::Object | super::TypeKind::Enum)
    }

    /// This class applied to its declared formals. A non-generic class is the bare classifier.
    pub(crate) fn self_type(&self) -> Ty {
        Ty::obj_args_name(self.classifier, &self.parameters.formals)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{KlibClassIdentity, TypeKind};

    #[test]
    fn final_dispatch_requires_the_exact_concrete_ordinary_class_contract() {
        let (mut contract, _) = test_fixture::generic_member_declaration();
        let identity = KlibClassIdentity {
            module: contract.module.clone(),
            signature: contract.signature.clone(),
        };
        let owner = contract.classifier;
        assert!(!contract.proves_final_class(&identity, owner));
        contract.modality = KotlinModality::Final;
        assert!(contract.proves_final_class(&identity, owner));
        let mut other_module = identity.clone();
        other_module.module.push_str("-other");
        assert!(!contract.proves_final_class(&other_module, owner));
        let mut other_class = identity.clone();
        other_class.signature =
            KlibPublicIdSignature::classifier(crate::types::type_name("other/Owner"));
        assert!(!contract.proves_final_class(&other_class, owner));
        assert!(!contract.proves_final_class(&identity, crate::types::type_name("other/Owner")));
        contract.is_expect = true;
        assert!(!contract.proves_final_class(&identity, owner));
        contract.is_expect = false;
        for kind in [TypeKind::Enum, TypeKind::Interface, TypeKind::Annotation] {
            contract.kind = kind;
            assert!(!contract.proves_final_class(&identity, owner), "{kind:?}");
        }
        contract.kind = TypeKind::Object;
        assert!(contract.proves_final_class(&identity, owner));
        contract.kind = TypeKind::Class;
        for modality in [
            KotlinModality::Open,
            KotlinModality::Abstract,
            KotlinModality::Sealed,
        ] {
            contract.modality = modality;
            assert!(
                !contract.proves_final_class(&identity, owner),
                "{modality:?}"
            );
        }
    }
}
