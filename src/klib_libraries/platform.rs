//! The provider as a compilation's semantic platform.
//!
//! Every hook answers from KLIB declarations or from Kotlin's own language classifiers, so the
//! same provider serves each target that reads its libraries from KLIBs.

use crate::libraries::{
    function_classifiers, AliasExpansion, DependencyAccessPolicy, LibraryCallable, SemanticPlatform,
};
use crate::symbol_source::SymbolSource;
use crate::types::{SemanticCallableOwner, Ty, TypeName};

use super::KlibLibraries;

const NATIVE_DEFAULT_IMPORT_PACKAGES: &[&str] = &["kotlin.native"];

impl DependencyAccessPolicy for KlibLibraries {
    fn internal_classifier_accessible(&self, owner: TypeName) -> bool {
        self.inventory.classifier_is_friend(owner)
    }

    fn internal_callable_accessible(
        &self,
        identity: Option<crate::fir::ExternalCallableId>,
        _owner: TypeName,
    ) -> bool {
        identity.is_some_and(|identity| self.identities.internal_friend(identity))
    }
}

impl SemanticPlatform for KlibLibraries {
    fn klib_dependency_body_facts(&self) -> Option<crate::libraries::DependencyBodyFacts<'_>> {
        Some(crate::libraries::DependencyBodyFacts::new(
            self.declaration_bodies(),
            self.inline_declaration_bodies(),
            self.reference_bindings(),
            self.intrinsic_facts(),
            self.dependency_body_snapshot(),
        ))
    }

    fn dependency_access_policy(&self) -> &dyn DependencyAccessPolicy {
        self
    }

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

    fn type_alias_expansion(&self, internal: TypeName) -> Option<AliasExpansion> {
        self.inventory.type_alias(internal).cloned()
    }

    fn is_default_library_owner(&self, internal: TypeName) -> bool {
        internal.starts_with("kotlin/")
    }

    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        // The provider remains target-neutral: the opened libraries themselves identify Native by
        // publishing the documented `kotlin.native` package. A Wasm stdlib has no such package and
        // therefore receives no Native-only wildcard import.
        if self
            .inventory
            .package_exists(crate::types::type_name("kotlin"), "native")
        {
            NATIVE_DEFAULT_IMPORT_PACKAGES
        } else {
            &[]
        }
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
