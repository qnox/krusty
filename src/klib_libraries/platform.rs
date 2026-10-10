//! The provider as a compilation's semantic platform.
//!
//! Every hook answers from KLIB declarations or from Kotlin's own language classifiers, so the
//! same provider serves each target that reads its libraries from KLIBs.

use crate::libraries::{function_classifiers, LibraryCallable, SemanticPlatform};
use crate::symbol_source::SymbolSource;
use crate::types::{SemanticCallableOwner, Ty, TypeName};

use super::KlibLibraries;

impl SemanticPlatform for KlibLibraries {
    fn function_type(&self, arity: usize) -> Option<Ty> {
        Some(function_classifiers::function_type(arity))
    }

    fn property_reference_type(&self, arity: usize, mutable: bool, args: &[Ty]) -> Option<Ty> {
        function_classifiers::property_reference_type(arity, mutable, args)
    }

    fn function_reference_type(&self, function: Ty) -> Option<Ty> {
        function_classifiers::function_reference_type(function)
    }

    fn class_literal_type(&self) -> Option<Ty> {
        Some(Ty::obj("kotlin/reflect/KClass"))
    }

    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        if ty.is_unsigned() {
            return ty.scalar_value_repr();
        }
        self.classifier(ty.obj_internal()?)
            .and_then(|classifier| classifier.value_underlying)
    }

    fn is_default_library_owner(&self, internal: TypeName) -> bool {
        internal.starts_with("kotlin/")
    }

    /// A KLIB top-level declaration is owned by its package; no container class realizes it.
    fn top_level_callable_package(&self, callable: &LibraryCallable) -> TypeName {
        match callable.declaration_owner {
            Some(SemanticCallableOwner::Package(package)) => package,
            Some(SemanticCallableOwner::Classifier(_)) | None => callable.owner,
        }
    }

    fn is_erased_contract_callable(&self, callable: &LibraryCallable) -> bool {
        crate::contracts::is_contract_intrinsic(callable, self.top_level_callable_package(callable))
    }

    fn contract_dsl_member(
        &self,
        callable: &crate::contracts::SelectedDslCallable<'_>,
    ) -> Option<crate::contracts::DslMember> {
        crate::contracts::dsl_member(callable)
    }
}
