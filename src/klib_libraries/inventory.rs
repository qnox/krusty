//! The merged declaration inventory of a set of KLIB libraries.

use crate::fx_hash::{HashMap, HashSet};

use super::classifier_signatures::{sign_package_classes, SignedClassifier};
use super::declaration_signatures::{
    sign_package_function, sign_package_property, KlibLibraryError, SignedFunction, SignedProperty,
};
use super::parameter_defaults::{DefaultSelection, ParameterDefaults};
use super::target_intrinsics::{KlibIntrinsicFacts, TargetIntrinsicPlatform};
use crate::libraries::{declared_type_alias, AliasExpansion};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::KlibIrSignature;
use crate::metadata::semantic::KotlinPackage;
use crate::types::{existing_type_name_child, type_name, type_name_child, TypeName, Visibility};

/// A companion extension (`companion fun C.name()`, `companion val C.name`), joined with the
/// package that declares it. It is named through the classifier `C` itself, not through a value of
/// `C`, so it belongs to `C`'s namespace rather than to its package's.
pub(super) struct CompanionExtension<T> {
    pub(super) package: TypeName,
    pub(super) signed: T,
}

/// One decoded package fragment with the archive authority needed while its declarations are
/// signed and normalized. Keeping these facts named prevents tuple position from becoming an
/// accidental provider contract as archive semantics grow.
pub(super) struct ArchivePackageInput {
    pub(super) segments: Vec<String>,
    pub(super) package: KotlinPackage,
    pub(super) compiler_builtin_owner: bool,
    pub(super) internal_friend: bool,
    pub(super) module: Option<String>,
    pub(super) intrinsic_platform: Option<TargetIntrinsicPlatform>,
    pub(super) native_interop_library: bool,
}

impl<T> CompanionExtension<T> {
    /// The semantic placement shared by namespace publication and exact dependency linking.
    pub(super) fn placement(&self, classifier: TypeName) -> crate::libraries::CallablePlacement {
        crate::libraries::CallablePlacement::Associated {
            classifier,
            declaration_owner: crate::types::SemanticCallableOwner::Package(self.package),
            access_owner: None,
        }
    }
}

/// Signed declarations by namespace, plus every package namespace a qualifier walk may pass
/// through.
pub(super) struct PackageInventory {
    functions: HashMap<TypeName, Vec<SignedFunction>>,
    properties: HashMap<TypeName, Vec<SignedProperty>>,
    classifiers: HashMap<TypeName, SignedClassifier>,
    type_aliases: HashMap<TypeName, AliasExpansion>,
    /// Companion extensions by the classifier they are named through.
    companion_functions: HashMap<TypeName, Vec<CompanionExtension<SignedFunction>>>,
    companion_properties: HashMap<TypeName, Vec<CompanionExtension<SignedProperty>>>,
    /// Each declared package and all of its enclosing packages. The root package is not listed:
    /// it is not a child of any namespace.
    namespaces: HashSet<TypeName>,
    intrinsics: KlibIntrinsicFacts,
}

/// Exact prepared-inline declarations selected by metadata, grouped by declaring module.
///
/// The prepared IR component can contain bodies that this compilation cannot publish. Keeping the
/// selection at the metadata inventory boundary lets archive decoding skip those bodies without
/// inferring eligibility from IR shape or source names.
#[derive(Default)]
pub(super) struct InlineBodySelection {
    by_module: HashMap<String, HashSet<KlibPublicIdSignature>>,
}

impl InlineBodySelection {
    fn insert(&mut self, module: Option<&str>, signature: &KlibPublicIdSignature) {
        let Some(module) = module else {
            return;
        };
        self.by_module
            .entry(module.to_owned())
            .or_default()
            .insert(signature.clone());
    }

    pub(super) fn contains(&self, module: &str, signature: &KlibIrSignature) -> bool {
        let KlibIrSignature::Public(signature) = signature else {
            return false;
        };
        self.by_module
            .get(module)
            .is_some_and(|selected| selected.contains(signature))
    }
}

impl PackageInventory {
    pub(super) fn from_packages(
        packages: Vec<(Vec<String>, KotlinPackage, bool)>,
    ) -> Result<Self, KlibLibraryError> {
        Self::from_package_inputs(
            packages
                .into_iter()
                .map(|(segments, package, compiler_builtin_owner)| {
                    (segments, package, compiler_builtin_owner, false, None)
                })
                .collect(),
        )
    }

    /// Build an inventory while retaining which archive supplied friend-visible internals.
    pub(super) fn from_package_inputs(
        packages: Vec<(Vec<String>, KotlinPackage, bool, bool, Option<String>)>,
    ) -> Result<Self, KlibLibraryError> {
        Self::from_target_package_inputs(
            packages
                .into_iter()
                .map(
                    |(segments, package, builtins, friend, module)| ArchivePackageInput {
                        segments,
                        package,
                        compiler_builtin_owner: builtins,
                        internal_friend: friend,
                        module,
                        intrinsic_platform: None,
                        native_interop_library: false,
                    },
                )
                .collect(),
        )
    }

    /// Build an archive inventory, retaining target-stdlib authority beside each fragment.
    pub(super) fn from_target_package_inputs(
        packages: Vec<ArchivePackageInput>,
    ) -> Result<Self, KlibLibraryError> {
        let mut inventory = Self {
            functions: HashMap::default(),
            properties: HashMap::default(),
            classifiers: HashMap::default(),
            type_aliases: HashMap::default(),
            companion_functions: HashMap::default(),
            companion_properties: HashMap::default(),
            namespaces: HashSet::default(),
            intrinsics: KlibIntrinsicFacts::default(),
        };
        // A type alias's qualified identity omits its declaring archive. When an ordinary
        // dependency and an exact friend both publish the same identity, keep the friend-owned
        // declaration just as classifier selection below does. Otherwise dependency ordering
        // could make an inaccessible alias shadow the declaration this compilation may use.
        let mut type_alias_friends = HashMap::default();
        // A public IdSignature is unique inside one KLIB, not across the dependency graph. Split
        // package fragments from the same module must not become duplicate candidates, while two
        // modules' equal internal signatures remain distinct so exact friendship can select one.
        let mut declared_signatures = HashSet::default();
        for ArchivePackageInput {
            segments,
            package,
            compiler_builtin_owner,
            internal_friend,
            module,
            intrinsic_platform,
            native_interop_library,
        } in packages
        {
            let identity = inventory.declare_package(&segments);
            for alias in package
                .type_aliases
                .into_iter()
                .filter(|alias| alias.visibility != Visibility::Private)
            {
                let expansion = declared_type_alias(identity, &alias, &Default::default())
                    .map_err(|error| {
                        super::declaration_signatures::unsignable(&segments, &alias.name, error)
                    })?;
                let selected = type_alias_friends
                    .get(&expansion.identity)
                    .is_none_or(|selected_friend| !selected_friend && internal_friend);
                if selected {
                    type_alias_friends.insert(expansion.identity, internal_friend);
                    inventory.type_aliases.insert(expansion.identity, expansion);
                }
            }
            let signed = package
                .functions
                .into_iter()
                // A private top-level function is visible only inside its own file; its library
                // serializes it under a file-local identity, never a public one.
                .filter(|function| function.visibility != Visibility::Private)
                .map(|function| {
                    sign_package_function(
                        &segments,
                        function,
                        internal_friend,
                        module.as_deref(),
                        native_interop_library,
                    )
                    .and_then(|mut signed| {
                        signed.compiler_builtin_owner = compiler_builtin_owner;
                        signed.direct_c_function = super::direct_c_calls::authenticate(
                            &segments,
                            native_interop_library,
                            crate::libraries::CallablePlacement::Package(identity),
                            &signed.declaration,
                        )?;
                        signed.target_intrinsic = super::target_intrinsics::authenticate(
                            intrinsic_platform,
                            crate::libraries::CallablePlacement::Package(identity),
                            &signed.declaration,
                        );
                        Ok(signed)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            for function in signed {
                inventory.intrinsics.insert(&function);
                if !declared_signatures.insert((module.clone(), function.signature.clone())) {
                    if function.compiler_builtin_owner {
                        inventory.insert_selected_function(identity, function);
                    }
                    continue;
                }
                inventory.insert_selected_function(identity, function);
            }
            let signed = package
                .properties
                .into_iter()
                // As for functions, a private top-level property has a file-local identity.
                .filter(|property| property.visibility != Visibility::Private)
                .map(|property| {
                    sign_package_property(
                        &segments,
                        property,
                        internal_friend,
                        module.as_deref(),
                        native_interop_library,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            for property in signed {
                if !declared_signatures.insert((module.clone(), property.signature.clone())) {
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
            for (classifier, mut signed) in sign_package_classes(
                &segments,
                package.classes,
                &mut declared_signatures,
                internal_friend,
                compiler_builtin_owner,
                module.as_deref(),
                native_interop_library,
            )? {
                let member = crate::libraries::CallablePlacement::Member {
                    owner: classifier,
                    owner_is_interface: signed.shape.is_interface(),
                };
                for function in &mut signed.functions {
                    function.direct_c_function = super::direct_c_calls::authenticate(
                        &segments,
                        native_interop_library,
                        member,
                        &function.declaration,
                    )?;
                    function.target_intrinsic = super::target_intrinsics::authenticate(
                        intrinsic_platform,
                        member,
                        &function.declaration,
                    );
                    inventory.intrinsics.insert(function);
                }
                for function in &mut signed.associated_functions {
                    let associated = crate::libraries::CallablePlacement::Associated {
                        classifier,
                        declaration_owner: crate::types::SemanticCallableOwner::Classifier(
                            classifier,
                        ),
                        access_owner: Some(classifier),
                    };
                    function.direct_c_function = super::direct_c_calls::authenticate(
                        &segments,
                        native_interop_library,
                        associated,
                        &function.declaration,
                    )?;
                    function.target_intrinsic = super::target_intrinsics::authenticate(
                        intrinsic_platform,
                        associated,
                        &function.declaration,
                    );
                    inventory.intrinsics.insert(function);
                }
                let selected = inventory
                    .classifiers
                    .get(&classifier)
                    .is_none_or(|current| !current.internal_friend && signed.internal_friend);
                if selected {
                    if let Some(previous) = inventory.classifiers.insert(classifier, signed) {
                        // Nested aliases are one facet of their owning classifier. Replacing an
                        // inaccessible same-FQN classifier must replace, not merge, those facets.
                        for alias in previous.type_aliases {
                            inventory.type_aliases.remove(&alias.identity);
                        }
                    }
                    for alias in &inventory.classifiers[&classifier].type_aliases {
                        inventory.type_aliases.insert(alias.identity, alias.clone());
                    }
                }
            }
        }
        Ok(inventory)
    }

    pub(super) fn intrinsic_facts(&self) -> &KlibIntrinsicFacts {
        &self.intrinsics
    }

    pub(super) fn attach_ir_intrinsics(
        &mut self,
        module: &str,
        platform: Option<TargetIntrinsicPlatform>,
        trees: &crate::metadata::klib_ir::KlibIrModuleTrees,
    ) {
        for (arena, function) in trees.all_functions() {
            self.intrinsics.insert_ir(module, platform, arena, function);
        }
    }

    /// Publish a signed function in the namespace selected by its own metadata.
    fn insert_function(&mut self, package: TypeName, function: SignedFunction) {
        if function.declaration.is_static {
            // A static top-level declaration is a companion extension; one without a classifier
            // receiver names nothing it could be called through.
            if let Some(classifier) = companion_receiver(&function.declaration.receiver) {
                self.companion_functions
                    .entry(classifier)
                    .or_default()
                    .push(CompanionExtension {
                        package,
                        signed: function,
                    });
            }
            return;
        }
        self.functions.entry(package).or_default().push(function);
    }

    /// Publish a function unless the compilation-designated stdlib already owns its signature.
    ///
    /// The public signature is the shared serialized identity, but it omits result type and source
    /// modifiers such as `operator`. Authority therefore belongs to the complete stdlib declaration,
    /// not just a boolean copied onto whichever dependency metadata happened to be read first.
    /// Removing from both publication stores also keeps the declaration in the namespace selected
    /// by its authoritative placement metadata.
    fn insert_selected_function(&mut self, package: TypeName, function: SignedFunction) {
        let signature = &function.signature;
        let authoritative_exists = self
            .functions
            .values()
            .flatten()
            .any(|prior| prior.compiler_builtin_owner && &prior.signature == signature)
            || self.companion_functions.values().flatten().any(|prior| {
                prior.signed.compiler_builtin_owner && &prior.signed.signature == signature
            });
        if authoritative_exists && !function.compiler_builtin_owner {
            return;
        }
        if function.compiler_builtin_owner {
            self.functions
                .values_mut()
                .for_each(|functions| functions.retain(|prior| &prior.signature != signature));
            self.functions.retain(|_, functions| !functions.is_empty());
            self.companion_functions.values_mut().for_each(|functions| {
                functions.retain(|prior| &prior.signed.signature != signature);
            });
            self.companion_functions
                .retain(|_, functions| !functions.is_empty());
        }
        self.insert_function(package, function);
    }

    /// The top-level declarations whose IR holds a parameter default that metadata says exists:
    /// a defaulted function, or a classifier with a defaulted constructor or member.
    pub(super) fn default_selection(&self) -> DefaultSelection {
        let mut wanted = DefaultSelection::default();
        let functions = self.functions.values().flatten().chain(
            self.companion_functions
                .values()
                .flatten()
                .map(|extension| &extension.signed),
        );
        for function in functions {
            if function.compiler_builtin_owner {
                wanted
                    .select_authoritative_function(function.module.as_deref(), &function.signature);
            }
            if function.declaration.param_defaults.contains(&true) {
                wanted.insert(function.module.as_deref(), &function.signature);
            }
        }
        for classifier in self.classifiers.values() {
            let constructors = classifier
                .constructors
                .iter()
                .filter(|constructor| constructor.declaration.param_defaults.contains(&true))
                .map(|constructor| &constructor.signature);
            let members = classifier
                .functions
                .iter()
                .chain(&classifier.associated_functions)
                .filter(|function| function.declaration.param_defaults.contains(&true))
                .map(|function| &function.signature);
            constructors
                .chain(members)
                .for_each(|signature| wanted.insert(classifier.module.as_deref(), signature));
        }
        wanted
    }

    /// Inline declarations, whose prepared bodies a call expands from as templates.
    pub(super) fn inline_body_selection(&self) -> InlineBodySelection {
        let mut selected = InlineBodySelection::default();
        let mut select = |function: &SignedFunction| {
            if function.declaration.is_inline {
                selected.insert(function.module.as_deref(), &function.signature);
            }
        };
        self.functions.values().flatten().for_each(&mut select);
        self.companion_functions
            .values()
            .flatten()
            .for_each(|extension| select(&extension.signed));
        for classifier in self.classifiers.values() {
            classifier.functions.iter().for_each(&mut select);
            classifier.associated_functions.iter().for_each(&mut select);
        }
        selected
    }

    /// Give every function the constant defaults its library's IR declares for it.
    pub(super) fn attach_defaults(&mut self, defaults: &ParameterDefaults) {
        let mut attach = |function: &mut SignedFunction| {
            let signature = KlibIrSignature::Public(function.signature.clone());
            function.defaults = defaults.of(function.module.as_deref(), &signature);
        };
        self.functions.values_mut().flatten().for_each(&mut attach);
        self.companion_functions
            .values_mut()
            .flatten()
            .for_each(|extension| attach(&mut extension.signed));
        for classifier in self.classifiers.values_mut() {
            for constructor in &mut classifier.constructors {
                let signature = KlibIrSignature::Public(constructor.signature.clone());
                constructor.defaults = defaults.of(classifier.module.as_deref(), &signature);
            }
            classifier.functions.iter_mut().for_each(&mut attach);
            classifier
                .associated_functions
                .iter_mut()
                .for_each(&mut attach);
        }
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

    /// The published function with this exact module-qualified serialized identity and its
    /// authoritative namespace placement.
    /// This is a reverse identity join for dependency-body linking, not a namespace/name lookup.
    pub(super) fn dependency_function(
        &self,
        identity: &crate::libraries::KlibDependencyCallable,
    ) -> Option<(crate::libraries::CallablePlacement, &SignedFunction)> {
        let crate::libraries::KlibDeclarationSignature::Public(signature) = &identity.declaration
        else {
            return None;
        };
        self.functions
            .iter()
            .find_map(|(&package, functions)| {
                functions
                    .iter()
                    .find(|function| {
                        function.module.as_deref() == Some(identity.module.as_str())
                            && &function.signature == signature
                    })
                    .map(|function| {
                        (
                            crate::libraries::CallablePlacement::Package(package),
                            function,
                        )
                    })
            })
            .or_else(|| {
                self.companion_functions
                    .iter()
                    .find_map(|(&classifier, extensions)| {
                        extensions
                            .iter()
                            .find(|extension| {
                                extension.signed.module.as_deref() == Some(identity.module.as_str())
                                    && &extension.signed.signature == signature
                            })
                            .map(|extension| (extension.placement(classifier), &extension.signed))
                    })
            })
            .or_else(|| {
                self.classifiers.iter().find_map(|(&classifier, signed)| {
                    signed
                        .functions
                        .iter()
                        .find(|function| {
                            function.module.as_deref() == Some(identity.module.as_str())
                                && &function.signature == signature
                        })
                        .map(|function| {
                            (
                                crate::libraries::CallablePlacement::Member {
                                    owner: classifier,
                                    owner_is_interface: signed.shape.is_interface(),
                                },
                                function,
                            )
                        })
                        .or_else(|| {
                            signed
                                .associated_functions
                                .iter()
                                .find(|function| {
                                    function.module.as_deref() == Some(identity.module.as_str())
                                        && &function.signature == signature
                                })
                                .map(|function| {
                                    (
                                        crate::libraries::CallablePlacement::Associated {
                                            classifier,
                                            declaration_owner:
                                                crate::types::SemanticCallableOwner::Classifier(
                                                    classifier,
                                                ),
                                            access_owner: Some(classifier),
                                        },
                                        function,
                                    )
                                })
                        })
                })
            })
    }

    /// The constructor serialized under exactly `identity`, with the class that declares it.
    pub(super) fn dependency_constructor(
        &self,
        identity: &crate::libraries::KlibDependencyCallable,
    ) -> Option<(
        TypeName,
        &SignedClassifier,
        &super::classifier_signatures::SignedConstructor,
    )> {
        let crate::libraries::KlibDeclarationSignature::Public(signature) = &identity.declaration
        else {
            return None;
        };
        self.classifiers
            .iter()
            .filter(|(_, signed)| signed.module.as_deref() == Some(identity.module.as_str()))
            .find_map(|(&classifier, signed)| {
                signed
                    .constructors
                    .iter()
                    .find(|constructor| &constructor.signature == signature)
                    .map(|constructor| (classifier, signed, constructor))
            })
    }

    /// The property whose getter or setter is serialized under exactly `identity`, at its
    /// placement, with the class that declares it as a member or companion-block member.
    pub(super) fn dependency_property(
        &self,
        identity: &crate::libraries::KlibDependencyCallable,
    ) -> Option<(
        crate::libraries::CallablePlacement,
        &SignedProperty,
        Option<&SignedClassifier>,
    )> {
        let crate::libraries::KlibDeclarationSignature::Accessor(accessor) = &identity.declaration
        else {
            return None;
        };
        let declares = |property: &&SignedProperty| {
            property.module.as_deref() == Some(identity.module.as_str())
                && (&property.getter == accessor || property.setter.as_ref() == Some(accessor))
        };
        self.properties
            .iter()
            .find_map(|(&package, properties)| {
                properties.iter().find(declares).map(|property| {
                    (
                        crate::libraries::CallablePlacement::Package(package),
                        property,
                        None,
                    )
                })
            })
            .or_else(|| {
                self.companion_properties
                    .iter()
                    .find_map(|(&classifier, extensions)| {
                        extensions
                            .iter()
                            .find(|extension| declares(&&extension.signed))
                            .map(|extension| {
                                (extension.placement(classifier), &extension.signed, None)
                            })
                    })
            })
            .or_else(|| {
                self.classifiers.iter().find_map(|(&classifier, signed)| {
                    signed
                        .properties
                        .iter()
                        .find(declares)
                        .map(|property| {
                            (
                                crate::libraries::CallablePlacement::Member {
                                    owner: classifier,
                                    owner_is_interface: signed.shape.is_interface(),
                                },
                                property,
                                Some(signed),
                            )
                        })
                        .or_else(|| {
                            signed
                                .associated_properties
                                .iter()
                                .find(declares)
                                .map(|property| {
                                    (
                                        super::classifier_records::block_member_placement(
                                            classifier,
                                        ),
                                        property,
                                        Some(signed),
                                    )
                                })
                        })
                })
            })
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

    /// Every published top-level property with its package, in no particular order.
    #[cfg(test)]
    pub(super) fn all_properties(&self) -> impl Iterator<Item = (TypeName, &SignedProperty)> {
        self.properties
            .iter()
            .flat_map(|(package, properties)| properties.iter().map(move |p| (*package, p)))
    }

    /// Every published class with its identity, in no particular order.
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

    /// Every companion extension property with the classifier it is named through.
    #[cfg(test)]
    pub(super) fn all_companion_properties(
        &self,
    ) -> impl Iterator<Item = (TypeName, &CompanionExtension<SignedProperty>)> {
        self.companion_properties
            .iter()
            .flat_map(|(classifier, extensions)| extensions.iter().map(move |e| (*classifier, e)))
    }

    /// The published class with exactly this identity.
    pub(super) fn classifier(&self, identity: TypeName) -> Option<&SignedClassifier> {
        self.classifiers.get(&identity)
    }

    /// Whether `identity` is declared by a friend KLIB selected for this compilation.
    pub(super) fn classifier_is_friend(&self, identity: TypeName) -> bool {
        self.classifiers
            .get(&identity)
            .is_some_and(|classifier| classifier.internal_friend)
    }

    /// The type alias declared with exactly this qualified identity.
    pub(super) fn type_alias(&self, identity: TypeName) -> Option<&AliasExpansion> {
        self.type_aliases.get(&identity)
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
