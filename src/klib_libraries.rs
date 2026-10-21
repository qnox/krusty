//! A target-neutral dependency provider over decoded KLIB metadata.
//!
//! Native and Wasm both read their libraries from KLIBs. This provider publishes those
//! declarations through the same [`SymbolSource`] boundary as every other provider, normalized into
//! the common library model, and tags each one with the exact public `IdSignature` that joins a
//! selected declaration to its serialized IR body. Nothing here names a target.
//!
//! Declarations are validated and signed once, when the provider is built. Conversion into
//! selection candidates is lazy and memoized per lookup key. [`KlibDeclarationBodies`] answers the
//! other half of that join: the decoded IR body serialized under a published signature.

mod archives;
mod builtin_realizations;
mod classifier_records;
mod classifier_signatures;
mod collection_roles;
mod declaration_bodies;
mod declaration_signatures;
mod direct_c_calls;
mod external_identities;
mod inventory;
mod lookup;
pub(crate) mod parameter_defaults;
mod platform;
mod serialized_references;
mod target_intrinsics;
#[cfg(test)]
mod tests;

use crate::metadata::semantic::KotlinPackage;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::TypeName;

pub(crate) use archives::OpenedKlibLibraries;
pub use archives::{KlibLibrariesOpenError, KlibLibraryInput, KlibStdlibSemantics};
pub(crate) use declaration_bodies::KlibCalleeOrigin;
pub use declaration_bodies::{KlibBodyLibrary, KlibDeclarationBodies, KlibDeclarationBodiesError};
pub use declaration_signatures::KlibLibraryError;
pub use serialized_references::{
    KlibReferenceBinding, KlibReferenceBindings, KlibReferenceDispatch, KlibReferenceOrigin,
};
pub use target_intrinsics::{KlibIntrinsicCallable, KlibIntrinsicFacts};

/// Declarations of a set of KLIB libraries, published as one symbol source.
///
/// The signed declarations are immutable and shared; the identities a compilation selects are its
/// own. [`KlibLibraries::for_compilation`] starts another compilation over the same declarations
/// without decoding or signing them again.
pub struct KlibLibraries {
    inventory: std::rc::Rc<inventory::PackageInventory>,
    bodies: std::rc::Rc<KlibDeclarationBodies>,
    /// The prepared-inline copies, the exclusive source of an inline function's cross-module body.
    inline_bodies: std::rc::Rc<KlibDeclarationBodies>,
    references: std::rc::Rc<KlibReferenceBindings>,
    body_snapshot: Option<crate::libraries::DependencyBodySnapshot>,
    identities: external_identities::ExternalIdentities,
    lookups: lookup::SymbolLookups,
}

impl KlibLibraries {
    /// Build the provider from decoded package fragments, each paired with its package's exact
    /// segments. Fragments of one package, from one library or several, are merged. Every
    /// declaration is signed here; a declaration that cannot be signed rejects the whole set.
    pub fn from_packages(
        packages: Vec<(Vec<String>, KotlinPackage)>,
    ) -> Result<Self, KlibLibraryError> {
        Ok(Self::over(std::rc::Rc::new(
            inventory::PackageInventory::from_packages(
                packages
                    .into_iter()
                    .map(|(segments, package)| (segments, package, false))
                    .collect(),
            )?,
        )))
    }

    /// Build one named in-memory KLIB provider. Test and compiler-owned synthetic libraries use
    /// this when their callable identities must join serialized bodies exactly as an archive's
    /// manifest `unique_name` would.
    #[cfg(test)]
    pub(crate) fn from_named_packages(
        module: &str,
        packages: Vec<(Vec<String>, KotlinPackage)>,
    ) -> Result<Self, KlibLibraryError> {
        Ok(Self::over(std::rc::Rc::new(
            inventory::PackageInventory::from_package_inputs(
                packages
                    .into_iter()
                    .map(|(segments, package)| {
                        (segments, package, false, false, Some(module.to_owned()))
                    })
                    .collect(),
            )?,
        )))
    }

    /// The same declarations, for another compilation with no selected identities yet.
    pub fn for_compilation(&self) -> Self {
        Self {
            inventory: std::rc::Rc::clone(&self.inventory),
            bodies: std::rc::Rc::clone(&self.bodies),
            inline_bodies: std::rc::Rc::clone(&self.inline_bodies),
            references: std::rc::Rc::clone(&self.references),
            body_snapshot: self.body_snapshot.clone(),
            identities: external_identities::ExternalIdentities::default(),
            lookups: lookup::SymbolLookups::default(),
        }
    }

    fn over(inventory: std::rc::Rc<inventory::PackageInventory>) -> Self {
        Self::over_with_bodies(
            inventory,
            std::rc::Rc::new(KlibDeclarationBodies::default()),
        )
    }

    fn over_with_bodies(
        inventory: std::rc::Rc<inventory::PackageInventory>,
        bodies: std::rc::Rc<KlibDeclarationBodies>,
    ) -> Self {
        Self::over_with_bodies_and_snapshot(
            inventory,
            bodies,
            std::rc::Rc::new(KlibDeclarationBodies::default()),
            None,
        )
    }

    fn over_with_bodies_and_snapshot(
        inventory: std::rc::Rc<inventory::PackageInventory>,
        bodies: std::rc::Rc<KlibDeclarationBodies>,
        inline_bodies: std::rc::Rc<KlibDeclarationBodies>,
        body_snapshot: Option<crate::libraries::DependencyBodySnapshot>,
    ) -> Self {
        let mut libraries = Self {
            inventory,
            bodies,
            inline_bodies,
            references: std::rc::Rc::default(),
            body_snapshot,
            identities: external_identities::ExternalIdentities::default(),
            lookups: lookup::SymbolLookups::default(),
        };
        libraries.references = std::rc::Rc::new(KlibReferenceBindings::freeze(&libraries));
        libraries
    }

    /// Intrinsic declarations authenticated from the compilation-selected target stdlib.
    pub fn intrinsic_facts(&self) -> &KlibIntrinsicFacts {
        self.inventory.intrinsic_facts()
    }

    /// Exact serialized bodies indexed from the same compilation inputs as the declarations.
    pub fn declaration_bodies(&self) -> &KlibDeclarationBodies {
        &self.bodies
    }

    /// The prepared-inline copies of the inline functions the same inputs declare.
    pub fn inline_declaration_bodies(&self) -> &KlibDeclarationBodies {
        &self.inline_bodies
    }

    /// Frozen serialized-reference bindings from the same archive generation as the bodies.
    pub fn reference_bindings(&self) -> &KlibReferenceBindings {
        &self.references
    }

    pub(crate) fn dependency_body_snapshot(
        &self,
    ) -> Option<&crate::libraries::DependencyBodySnapshot> {
        self.body_snapshot.as_ref()
    }
}

impl SymbolSource for KlibLibraries {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        self.inventory.package_exists(parent, name)
    }

    fn symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        self.lookups.symbols(namespace, name, || {
            lookup::declared_symbols(&self.inventory, &self.identities, namespace, name)
        })
    }

    fn external_callable(
        &self,
        identity: crate::fir::ExternalCallableId,
    ) -> Option<crate::libraries::ExternalCallableRealization> {
        let mut realization = self.identities.realization(identity)?;
        if let Some((owner, class)) = realization
            .dependency_callable
            .as_ref()
            .and_then(|identity| self.inventory.dependency_declaring_class(identity))
        {
            realization.klib_body_class_contract = Some(crate::libraries::KlibBodyClassContract {
                module: class.module.clone()?,
                signature: class.signature.clone(),
                classifier: owner,
                parameters: class.type_parameters.klib_body_parameters(),
            });
        }
        Some(realization)
    }

    fn external_callable_by_dependency(
        &self,
        identity: &crate::libraries::KlibDependencyCallable,
    ) -> Option<(
        crate::fir::ExternalCallableId,
        crate::libraries::ExternalCallableRealization,
    )> {
        // A serialized identity names one declaration: a function, a property accessor or a
        // constructor.
        let external =
            if let Some((placement, signed)) = self.inventory.dependency_function(identity) {
                let package = crate::libraries::EnclosingBounds::default();
                let enclosing = self
                    .inventory
                    .dependency_declaring_class(identity)
                    .map_or(&package, |(_, class)| class.type_parameters.enclosing());
                lookup::published_function(&self.identities, signed, placement, enclosing)
                    .callable
                    .external_identity?
            } else if let Some((placement, signed, classifier)) =
                self.inventory.dependency_property(identity)
            {
                let package = crate::libraries::EnclosingBounds::default();
                let enclosing = classifier.map_or(&package, |classifier| {
                    classifier.type_parameters.enclosing()
                });
                let property =
                    lookup::published_property(&self.identities, signed, placement, enclosing);
                // The accessor the identity serializes; the realization check below proves it.
                let setter = signed.setter.as_ref().is_some_and(|setter| {
                    identity.declaration
                        == crate::libraries::KlibDeclarationSignature::Accessor(setter.clone())
                });
                if setter {
                    property.setter?.external_identity?
                } else {
                    property.getter.external_identity?
                }
            } else {
                let (owner, classifier, constructor) =
                    self.inventory.dependency_constructor(identity)?;
                classifier_records::published_constructor(
                    &self.identities,
                    owner,
                    classifier,
                    constructor,
                )
                .external_identity?
            };
        let realization = self.external_callable(external)?;
        (realization.dependency_callable.as_ref() == Some(identity))
            .then_some((external, realization))
    }

    fn is_platform_stdlib_dependency(
        &self,
        identity: &crate::libraries::KlibDependencyCallable,
    ) -> bool {
        self.inventory
            .dependency_function(identity)
            .is_some_and(|(_, function)| function.compiler_builtin_owner)
    }

    fn external_property(
        &self,
        identity: crate::fir::ExternalPropertyId,
    ) -> Option<crate::libraries::ExternalPropertyRealization> {
        self.identities.property(identity)
    }
}
