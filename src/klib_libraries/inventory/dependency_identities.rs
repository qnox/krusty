//! Where the inventory publishes each declaration a dependency body links by exact identity.
//!
//! A serialized body names a callee by its declaring module and public signature. Linking it asks
//! the inventory for the published function, property, constructor or declaring class with that
//! identity. The index answers those questions by map lookup; it is built from the finished
//! inventory on the first such question, so publication order and withdrawal by the stdlib are
//! already settled.

use crate::fx_hash::HashMap;
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::types::TypeName;

use super::PackageInventory;

/// Where one published function or property is stored, in the inventory's own keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Store {
    /// The `index`th declaration `package` publishes.
    Package { package: TypeName, index: usize },
    /// The `index`th companion extension named through `classifier`.
    Companion { classifier: TypeName, index: usize },
    /// The `index`th member of `classifier`.
    Member { classifier: TypeName, index: usize },
    /// The `index`th companion-block member of `classifier`.
    Associated { classifier: TypeName, index: usize },
}

/// Where one published function is stored: a store functions share with properties, or the
/// private top-level externals, which only functions have.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FunctionStore {
    Shared(Store),
    /// The `index`th private top-level external.
    PrivateExternal {
        index: usize,
    },
}

/// Entries by a declaration's exact identity: its declaring module, then its serialized signature.
type ByIdentity<S, T> = HashMap<String, HashMap<S, Vec<T>>>;

/// The entries of the identity `module` and `signature`, created empty.
fn entries<'a, S: Eq + std::hash::Hash + Clone, T>(
    map: &'a mut ByIdentity<S, T>,
    module: &str,
    signature: &S,
) -> &'a mut Vec<T> {
    map.entry(module.to_owned())
        .or_default()
        .entry(signature.clone())
        .or_default()
}

#[derive(Default)]
pub(super) struct DependencyIdentities {
    /// Each function, in the order the inventory prefers its stores: package, private external,
    /// companion extension, then class member before companion-block member.
    functions: ByIdentity<KlibPublicIdSignature, FunctionStore>,
    /// Each property by either accessor, in the same store order without private externals.
    properties: ByIdentity<KlibAccessorIdSignature, Store>,
    /// Each class constructor, with the index of the constructor in its class.
    constructors: ByIdentity<KlibPublicIdSignature, (TypeName, usize)>,
    /// The classes that declare a member function or constructor, by the class's module.
    function_owners: ByIdentity<KlibPublicIdSignature, TypeName>,
    /// The classes that declare a member property accessor, by the class's module.
    accessor_owners: ByIdentity<KlibAccessorIdSignature, TypeName>,
    /// The enum classes whose implicit `values`, `valueOf` or `entries` getter carries the
    /// signature, by the class's module. Metadata leaves those members implicit; their signatures
    /// are the ones the class publishes for them.
    enum_members: ByIdentity<KlibDeclarationSignature, TypeName>,
}

impl DependencyIdentities {
    pub(super) fn of(inventory: &PackageInventory) -> Self {
        let mut index = Self::default();
        let mut function = |module: &Option<String>, signature: &KlibPublicIdSignature, at| {
            if let Some(module) = module {
                entries(&mut index.functions, module, signature).push(at);
            }
        };
        for (&package, functions) in &inventory.functions {
            for (index, signed) in functions.iter().enumerate() {
                function(
                    &signed.module,
                    &signed.signature,
                    FunctionStore::Shared(Store::Package { package, index }),
                );
            }
        }
        for (index, (_, signed)) in inventory.private_externals.iter().enumerate() {
            function(
                &signed.module,
                &signed.signature,
                FunctionStore::PrivateExternal { index },
            );
        }
        for (&classifier, extensions) in &inventory.companion_functions {
            for (index, extension) in extensions.iter().enumerate() {
                function(
                    &extension.signed.module,
                    &extension.signed.signature,
                    FunctionStore::Shared(Store::Companion { classifier, index }),
                );
            }
        }
        for (&classifier, class) in &inventory.classifiers {
            for (index, signed) in class.functions.iter().enumerate() {
                function(
                    &signed.module,
                    &signed.signature,
                    FunctionStore::Shared(Store::Member { classifier, index }),
                );
            }
            for (index, signed) in class.associated_functions.iter().enumerate() {
                function(
                    &signed.module,
                    &signed.signature,
                    FunctionStore::Shared(Store::Associated { classifier, index }),
                );
            }
        }

        let mut property =
            |module: &Option<String>, accessors: [Option<&KlibAccessorIdSignature>; 2], at| {
                let Some(module) = module else {
                    return;
                };
                for accessor in accessors.into_iter().flatten() {
                    let stores = entries(&mut index.properties, module, accessor);
                    if stores.last() != Some(&at) {
                        stores.push(at);
                    }
                }
            };
        for (&package, properties) in &inventory.properties {
            for (index, signed) in properties.iter().enumerate() {
                property(
                    &signed.module,
                    [Some(&signed.getter), signed.setter.as_ref()],
                    Store::Package { package, index },
                );
            }
        }
        for (&classifier, extensions) in &inventory.companion_properties {
            for (index, extension) in extensions.iter().enumerate() {
                let signed = &extension.signed;
                property(
                    &signed.module,
                    [Some(&signed.getter), signed.setter.as_ref()],
                    Store::Companion { classifier, index },
                );
            }
        }
        for (&classifier, class) in &inventory.classifiers {
            for (index, signed) in class.properties.iter().enumerate() {
                property(
                    &signed.module,
                    [Some(&signed.getter), signed.setter.as_ref()],
                    Store::Member { classifier, index },
                );
            }
            for (index, signed) in class.associated_properties.iter().enumerate() {
                property(
                    &signed.module,
                    [Some(&signed.getter), signed.setter.as_ref()],
                    Store::Associated { classifier, index },
                );
            }
        }

        for (&classifier, class) in &inventory.classifiers {
            let Some(module) = &class.module else {
                continue;
            };
            for (position, constructor) in class.declared_constructors().enumerate() {
                entries(&mut index.constructors, module, &constructor.signature)
                    .push((classifier, position));
            }
            let signatures = class
                .functions
                .iter()
                .map(|function| &function.signature)
                .chain(
                    class
                        .declared_constructors()
                        .map(|constructor| &constructor.signature),
                );
            for signature in signatures {
                let owners = entries(&mut index.function_owners, module, signature);
                if owners.last() != Some(&classifier) {
                    owners.push(classifier);
                }
            }
            let accessors = class.properties.iter().flat_map(|property| {
                std::iter::once(&property.getter).chain(property.setter.as_ref())
            });
            for accessor in accessors {
                let owners = entries(&mut index.accessor_owners, module, accessor);
                if owners.last() != Some(&classifier) {
                    owners.push(classifier);
                }
            }
            if let Some(members) = &class.enum_members {
                let signatures = [
                    Some(KlibDeclarationSignature::Public(members.values.clone())),
                    Some(KlibDeclarationSignature::Public(members.value_of.clone())),
                    members
                        .entries_getter
                        .clone()
                        .map(KlibDeclarationSignature::Accessor),
                ];
                for signature in signatures.into_iter().flatten() {
                    entries(&mut index.enum_members, module, &signature).push(classifier);
                }
            }
        }
        index
    }

    /// The store of the function `module` serializes under `signature`.
    pub(super) fn function(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Option<FunctionStore> {
        first(&self.functions, module, signature)
    }

    /// The store of the property `module` serializes with the accessor `accessor`.
    pub(super) fn property(
        &self,
        module: &str,
        accessor: &KlibAccessorIdSignature,
    ) -> Option<Store> {
        first(&self.properties, module, accessor)
    }

    /// The class of `module` with a constructor serialized under `signature`, and its position among
    /// the class's declared constructors.
    pub(super) fn constructor(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Option<(TypeName, usize)> {
        first(&self.constructors, module, signature)
    }

    /// The class of `module` that declares the member function or constructor `signature`.
    pub(super) fn function_owner(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Option<TypeName> {
        first(&self.function_owners, module, signature)
    }

    /// The class of `module` that declares the member property accessor `accessor`.
    pub(super) fn accessor_owner(
        &self,
        module: &str,
        accessor: &KlibAccessorIdSignature,
    ) -> Option<TypeName> {
        first(&self.accessor_owners, module, accessor)
    }

    /// The enum class of `module` whose implicit member is serialized under `signature`.
    pub(super) fn enum_member(
        &self,
        module: &str,
        signature: &KlibDeclarationSignature,
    ) -> Option<TypeName> {
        first(&self.enum_members, module, signature)
    }
}

fn first<S: Eq + std::hash::Hash, T: Copy>(
    map: &ByIdentity<S, T>,
    module: &str,
    signature: &S,
) -> Option<T> {
    map.get(module)?.get(signature)?.first().copied()
}
