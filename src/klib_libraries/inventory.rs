//! The merged declaration inventory of a set of KLIB libraries.

use std::collections::{HashMap, HashSet};

use super::classifier_signatures::{sign_package_classes, SignedClassifier};
use super::declaration_signatures::{
    sign_package_function, sign_package_property, KlibLibraryError, SignedFunction, SignedProperty,
};
use crate::metadata::semantic::KotlinPackage;
use crate::types::{existing_type_name_child, type_name, type_name_child, TypeName, Visibility};

/// A companion extension (`companion fun C.name()`, `companion val C.name`), joined with the
/// package that declares it. It is named through the classifier `C` itself, not through a value of
/// `C`, so it belongs to `C`'s namespace rather than to its package's.
pub(super) struct CompanionExtension<T> {
    pub(super) package: TypeName,
    pub(super) signed: T,
}

/// Signed declarations by namespace, plus every package namespace a qualifier walk may pass
/// through.
pub(super) struct PackageInventory {
    functions: HashMap<TypeName, Vec<SignedFunction>>,
    properties: HashMap<TypeName, Vec<SignedProperty>>,
    classifiers: HashMap<TypeName, SignedClassifier>,
    /// Companion extensions by the classifier they are named through.
    companion_functions: HashMap<TypeName, Vec<CompanionExtension<SignedFunction>>>,
    companion_properties: HashMap<TypeName, Vec<CompanionExtension<SignedProperty>>>,
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
            properties: HashMap::new(),
            classifiers: HashMap::new(),
            companion_functions: HashMap::new(),
            companion_properties: HashMap::new(),
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
            for function in signed {
                if !declared_signatures.insert(function.signature.clone()) {
                    continue;
                }
                if function.declaration.is_static {
                    // A static top-level declaration is a companion extension; one without a
                    // classifier receiver names nothing it could be called through.
                    if let Some(classifier) = companion_receiver(&function.declaration.receiver) {
                        inventory
                            .companion_functions
                            .entry(classifier)
                            .or_default()
                            .push(CompanionExtension {
                                package: identity,
                                signed: function,
                            });
                    }
                    continue;
                }
                inventory
                    .functions
                    .entry(identity)
                    .or_default()
                    .push(function);
            }
            let signed = package
                .properties
                .into_iter()
                // As for functions, a private top-level property has a file-local identity.
                .filter(|property| property.visibility != Visibility::Private)
                .map(|property| sign_package_property(&segments, property))
                .collect::<Result<Vec<_>, _>>()?;
            for property in signed {
                if !declared_signatures.insert(property.signature.clone()) {
                    continue;
                }
                if property.declaration.is_static {
                    // A static top-level declaration is a companion extension; one without a
                    // classifier receiver names nothing it could be called through.
                    if let Some(classifier) = companion_receiver(&property.declaration.receiver) {
                        inventory
                            .companion_properties
                            .entry(classifier)
                            .or_default()
                            .push(CompanionExtension {
                                package: identity,
                                signed: property,
                            });
                    }
                    continue;
                }
                inventory
                    .properties
                    .entry(identity)
                    .or_default()
                    .push(property);
            }
            for (classifier, signed) in
                sign_package_classes(&segments, package.classes, &mut declared_signatures)?
            {
                inventory.classifiers.insert(classifier, signed);
            }
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

    /// The top-level properties `package` declares under `name`, in library order.
    pub(super) fn properties<'a>(
        &'a self,
        package: TypeName,
        name: &'a str,
    ) -> impl Iterator<Item = &'a SignedProperty> {
        self.properties
            .get(&package)
            .into_iter()
            .flatten()
            .filter(move |property| property.declaration.name == name)
    }

    /// Every published top-level function with its package, in no particular order.
    #[cfg(test)]
    pub(super) fn all_functions(&self) -> impl Iterator<Item = (TypeName, &SignedFunction)> {
        self.functions
            .iter()
            .flat_map(|(package, functions)| functions.iter().map(move |f| (*package, f)))
    }

    /// Every published class with its identity, in no particular order.
    #[cfg(test)]
    pub(super) fn all_classifiers(&self) -> impl Iterator<Item = (TypeName, &SignedClassifier)> {
        self.classifiers
            .iter()
            .map(|(identity, classifier)| (*identity, classifier))
    }

    /// Every companion extension function with the classifier it is named through.
    #[cfg(test)]
    pub(super) fn all_companion_functions(
        &self,
    ) -> impl Iterator<Item = (TypeName, &CompanionExtension<SignedFunction>)> {
        self.companion_functions
            .iter()
            .flat_map(|(classifier, extensions)| extensions.iter().map(move |e| (*classifier, e)))
    }

    /// The published class with exactly this identity.
    pub(super) fn classifier(&self, identity: TypeName) -> Option<&SignedClassifier> {
        self.classifiers.get(&identity)
    }

    /// The companion extension functions named `name` that are named through `classifier`.
    pub(super) fn companion_functions<'a>(
        &'a self,
        classifier: TypeName,
        name: &'a str,
    ) -> impl Iterator<Item = &'a CompanionExtension<SignedFunction>> {
        self.companion_functions
            .get(&classifier)
            .into_iter()
            .flatten()
            .filter(move |extension| extension.signed.declaration.name == name)
    }

    /// The companion extension properties named `name` that are named through `classifier`.
    pub(super) fn companion_properties<'a>(
        &'a self,
        classifier: TypeName,
        name: &'a str,
    ) -> impl Iterator<Item = &'a CompanionExtension<SignedProperty>> {
        self.companion_properties
            .get(&classifier)
            .into_iter()
            .flatten()
            .filter(move |extension| extension.signed.declaration.name == name)
    }
}

/// The classifier a companion extension names through its receiver; signing proved the receiver
/// a plain class type.
fn companion_receiver(
    receiver: &Option<crate::metadata::semantic::KotlinType>,
) -> Option<TypeName> {
    receiver
        .as_ref()
        .and_then(|receiver| receiver.internal())
        .map(type_name)
}
