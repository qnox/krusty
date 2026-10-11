//! KLIB libraries layered after another semantic platform.
//!
//! A Native compilation still reads the standard library through the platform it was given, while
//! the libraries a build adds as KLIBs (an interop library that `cinterop` wrote, for one) are
//! published by this provider. The overlay is one [`SemanticPlatform`]: declarations federate as a
//! [`CompositeSource`] does, the base first, and every platform rule is the base's.
//!
//! Each provider numbers the identities it assigns densely from zero, so the layered libraries
//! number theirs from [`LAYER_BASE`]. An identity is answered by the provider that assigned it,
//! decided by its range rather than by asking each provider in turn.

use std::path::PathBuf;

use super::{KlibLibraries, KlibLibrariesOpenError};
use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::libraries::{
    AliasExpansion, ExternalCallableRealization, ExternalPropertyRealization, LibraryCallable,
    LibraryMember, PlatformInitializationError, PlatformSourceHeaderInput, PropertyInfo,
    PropertySet, ResolvedSymbols, SemanticPlatform, SemanticSupertype, SourceHeaderError,
};
use crate::symbol_source::{CompositeSource, SymbolNamespace, SymbolSource};
use crate::types::{Ty, TypeName};

/// The first identity the layered libraries assign. A base provider assigns identities densely
/// from zero and never reaches it.
const LAYER_BASE: u32 = 1 << 31;

/// A base platform with KLIB libraries published after it.
pub struct KlibOverlay {
    base: Box<dyn SemanticPlatform>,
    libraries: KlibLibraries,
}

impl KlibOverlay {
    /// Publish the KLIBs at `libraries` after `base`.
    pub fn open(
        base: Box<dyn SemanticPlatform>,
        libraries: &[PathBuf],
    ) -> Result<Self, KlibLibrariesOpenError> {
        let libraries = KlibLibraries::open(libraries)?.with_identities_from(LAYER_BASE);
        Ok(Self { base, libraries })
    }

    fn layered(raw: u32) -> bool {
        raw >= LAYER_BASE
    }

    fn sources(&self) -> CompositeSource<'_> {
        CompositeSource::new(vec![self.base.as_ref(), &self.libraries])
    }

    /// The provider that published `callable`.
    fn owner_of(&self, callable: &LibraryCallable) -> &dyn SemanticPlatform {
        match callable.external_identity {
            Some(identity) if Self::layered(identity.raw()) => &self.libraries,
            _ => self.base.as_ref(),
        }
    }
}

impl SymbolSource for KlibOverlay {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.sources().package_exists(parent, name)
    }

    fn api_withheld_callable(&self, owner: TypeName, name: &str) -> bool {
        self.sources().api_withheld_callable(owner, name)
    }

    fn symbols(&self, namespace: SymbolNamespace, name: &str) -> std::rc::Rc<ResolvedSymbols> {
        self.sources().symbols(namespace, name)
    }

    fn external_callable(
        &self,
        identity: ExternalCallableId,
    ) -> Option<ExternalCallableRealization> {
        if Self::layered(identity.raw()) {
            self.libraries.external_callable(identity)
        } else {
            self.base.external_callable(identity)
        }
    }

    fn external_property(
        &self,
        identity: ExternalPropertyId,
    ) -> Option<ExternalPropertyRealization> {
        if Self::layered(identity.raw()) {
            self.libraries.external_property(identity)
        } else {
            self.base.external_property(identity)
        }
    }

    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        self.base.platform_flexible_upper_bound(lower)
    }

    fn generated_serializer_singleton(&self, classifier: TypeName) -> Option<TypeName> {
        self.base.generated_serializer_singleton(classifier)
    }
}

impl SemanticPlatform for KlibOverlay {
    fn validate_initialization(&self) -> Result<(), PlatformInitializationError> {
        self.base.validate_initialization()
    }

    fn install_source_module_headers(
        &self,
        sources: &[PlatformSourceHeaderInput<'_>],
        source_classifiers: &[TypeName],
    ) -> Result<(), SourceHeaderError> {
        self.base
            .install_source_module_headers(sources, source_classifiers)
    }

    fn is_optional_expectation(&self, classifier: TypeName) -> bool {
        self.base.is_optional_expectation(classifier)
    }

    fn internal_accessible(&self, owner: TypeName) -> bool {
        self.base.internal_accessible(owner)
    }

    fn function_type(&self, arity: usize) -> Option<Ty> {
        self.base.function_type(arity)
    }

    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.base
            .value_underlying(ty)
            .or_else(|| self.libraries.value_underlying(ty))
    }

    fn inherits_classifier_callables(&self, internal: TypeName) -> bool {
        self.base.inherits_classifier_callables(internal)
    }

    fn top_level_associated_property(&self, package: TypeName, name: &str) -> Option<PropertyInfo> {
        self.base.top_level_associated_property(package, name)
    }

    fn external_property_diagnostic_label(
        &self,
        property: ExternalPropertyId,
        name: &str,
        ty: Ty,
    ) -> Option<String> {
        if Self::layered(property.raw()) {
            return None;
        }
        self.base
            .external_property_diagnostic_label(property, name, ty)
    }

    fn diagnostic_target_name(&self) -> Option<&str> {
        self.base.diagnostic_target_name()
    }

    fn library_value_form(&self, ty: Ty) -> Ty {
        self.base.library_value_form(ty)
    }

    fn library_value_form_name(&self, internal: TypeName) -> TypeName {
        self.base.library_value_form_name(internal)
    }

    fn canonical_source_type_name(&self, internal: TypeName) -> TypeName {
        self.base.canonical_source_type_name(internal)
    }

    fn type_alias_expansion(&self, internal: TypeName) -> Option<AliasExpansion> {
        self.base
            .type_alias_expansion(internal)
            .or_else(|| self.libraries.type_alias_expansion(internal))
    }

    fn is_default_library_owner(&self, internal: TypeName) -> bool {
        self.base.is_default_library_owner(internal)
    }

    fn is_erased_contract_callable(&self, callable: &LibraryCallable) -> bool {
        self.owner_of(callable)
            .is_erased_contract_callable(callable)
    }

    fn top_level_callable_package(&self, callable: &LibraryCallable) -> TypeName {
        self.owner_of(callable).top_level_callable_package(callable)
    }

    fn contract_dsl_member(
        &self,
        callable: &crate::contracts::SelectedDslCallable<'_>,
    ) -> Option<crate::contracts::DslMember> {
        self.base.contract_dsl_member(callable)
    }

    fn boxed_primitive(&self, ty: Ty) -> Option<Ty> {
        self.base.boxed_primitive(ty)
    }

    fn reference_primitive(&self, ty: Ty) -> Option<Ty> {
        self.base.reference_primitive(ty)
    }

    fn function_like_arity(&self, ty: Ty) -> Option<usize> {
        self.base.function_like_arity(ty)
    }

    fn property_reference_type(&self, arity: usize, mutable: bool, args: &[Ty]) -> Option<Ty> {
        self.base.property_reference_type(arity, mutable, args)
    }

    fn function_reference_type(&self, function: Ty) -> Option<Ty> {
        self.base.function_reference_type(function)
    }

    fn class_literal_type(&self) -> Option<Ty> {
        self.base.class_literal_type()
    }

    fn intrinsic_property(&self, receiver: Ty, name: &str) -> Option<LibraryMember> {
        self.base.intrinsic_property(receiver, name)
    }

    fn implicit_common_supertypes(&self, types: &[Ty]) -> Vec<SemanticSupertype> {
        self.base.implicit_common_supertypes(types)
    }

    fn platform_default_import_packages(&self) -> &'static [&'static str] {
        self.base.platform_default_import_packages()
    }

    fn inherited_accessor_properties(
        &self,
        source: &dyn SymbolSource,
        receiver: Ty,
        property: &str,
    ) -> PropertySet {
        self.base
            .inherited_accessor_properties(source, receiver, property)
    }

    fn builtin_type_internal(&self, simple_name: &str) -> Option<String> {
        self.base.builtin_type_internal(simple_name)
    }

    fn signature_formal_names(&self, signature: &str) -> Vec<String> {
        self.base.signature_formal_names(signature)
    }
}
