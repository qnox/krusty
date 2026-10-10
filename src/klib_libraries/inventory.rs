//! The merged package inventory of a set of KLIB libraries.

use std::collections::{HashMap, HashSet};

use super::declaration_signatures::{sign_package_function, KlibLibraryError, SignedFunction};
use crate::metadata::semantic::KotlinPackage;
use crate::types::{existing_type_name_child, type_name_child, TypeName, Visibility};

/// Signed declarations by package, plus every package namespace a qualifier walk may pass through.
pub(super) struct PackageInventory {
    functions: HashMap<TypeName, Vec<SignedFunction>>,
    /// Each declared package and all of its enclosing packages. The root package is not listed:
    /// it is not a child of any namespace.
    namespaces: HashSet<TypeName>,
}

impl PackageInventory {
    pub(super) fn from_packages(
        packages: Vec<(Vec<String>, KotlinPackage)>,
    ) -> Result<Self, KlibLibraryError> {
        let mut inventory = Self {
            functions: HashMap::new(),
            namespaces: HashSet::new(),
        };
        // A public IdSignature is the serialized declaration identity used to join metadata to
        // one IR body. Split package fragments, or repeated libraries in the input graph, may
        // publish the same declaration more than once; they must not become duplicate overload
        // candidates carrying one ExternalCallableId.
        let mut declared_signatures = HashSet::new();
        for (segments, package) in packages {
            let identity = inventory.declare_package(&segments);
            let signed = package
                .functions
                .into_iter()
                // A private top-level function is visible only inside its own file; its library
                // serializes it under a file-local identity, never a public one.
                .filter(|function| function.visibility != Visibility::Private)
                .map(|function| sign_package_function(&segments, function))
                .collect::<Result<Vec<_>, _>>()?;
            inventory.functions.entry(identity).or_default().extend(
                signed
                    .into_iter()
                    .filter(|function| declared_signatures.insert(function.signature.clone())),
            );
        }
        Ok(inventory)
    }

    fn declare_package(&mut self, segments: &[String]) -> TypeName {
        segments.iter().fold(TypeName::ROOT, |parent, segment| {
            let package = type_name_child(parent, segment);
            self.namespaces.insert(package);
            package
        })
    }

    pub(super) fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        existing_type_name_child(parent, name).is_some_and(|child| self.namespaces.contains(&child))
    }

    /// The top-level functions `package` declares under `name`, in library order.
    pub(super) fn functions<'a>(
        &'a self,
        package: TypeName,
        name: &'a str,
    ) -> impl Iterator<Item = &'a SignedFunction> {
        self.functions
            .get(&package)
            .into_iter()
            .flatten()
            .filter(move |function| function.declaration.name == name)
    }
}
