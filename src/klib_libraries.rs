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
mod declaration_bodies;
mod declaration_signatures;
mod external_identities;
mod inventory;
mod lookup;
pub(crate) mod parameter_defaults;
mod platform;
#[cfg(test)]
mod tests;

use crate::metadata::semantic::KotlinPackage;
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::TypeName;

pub use archives::KlibLibrariesOpenError;
pub use declaration_bodies::{KlibDeclarationBodies, KlibDeclarationBodiesError};
pub use declaration_signatures::KlibLibraryError;

/// Declarations of a set of KLIB libraries, published as one symbol source.
///
/// The signed declarations are immutable and shared; the identities a compilation selects are its
/// own. [`KlibLibraries::for_compilation`] starts another compilation over the same declarations
/// without decoding or signing them again.
pub struct KlibLibraries {
    inventory: std::rc::Rc<inventory::PackageInventory>,
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
            inventory::PackageInventory::from_packages(packages)?,
        )))
    }

    /// The same declarations, for another compilation with no selected identities yet.
    pub fn for_compilation(&self) -> Self {
        Self::over(std::rc::Rc::clone(&self.inventory))
    }

    fn over(inventory: std::rc::Rc<inventory::PackageInventory>) -> Self {
        Self {
            inventory,
            identities: external_identities::ExternalIdentities::default(),
            lookups: lookup::SymbolLookups::default(),
        }
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
        self.identities.realization(identity)
    }

    fn external_property(
        &self,
        identity: crate::fir::ExternalPropertyId,
    ) -> Option<crate::libraries::ExternalPropertyRealization> {
        self.identities.property(identity)
    }
}
