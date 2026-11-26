//! The serialized IR bodies of the declarations a set of KLIB libraries publishes.
//!
//! A selected dependency callable reaches its body through the exact serialized identity its
//! provider published with it (`KlibDependencyCallable`), never through a name, an owner, or a
//! parameter tuple. A public signature is unique only inside one KLIB, so every body key includes
//! the declaring module's manifest `unique_name`. Calls inside a serialized body recover that
//! module only from the producer's manifest dependency graph; current-compilation input roles and
//! unrelated declarations are not linkage evidence.

use std::collections::VecDeque;

use crate::fx_hash::{HashMap, HashSet};

use crate::klib::KlibManifest;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrBody, KlibIrClass, KlibIrExprKind, KlibIrFunction, KlibIrMember,
    KlibIrStatement, KlibIrType, KlibIrTypeParameter,
};
use crate::metadata::klib_ir::{
    KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind,
};
use crate::types::{type_name_child, TypeName};

mod inheritance;
pub(crate) use inheritance::member_signature;

/// The decoded function declarations of a set of KLIB libraries, joined by exact signature.
#[derive(Default)]
pub struct KlibDeclarationBodies {
    /// Decoded trees are immutable and may be shared with other compilations over the same archive.
    libraries: Vec<std::sync::Arc<KlibIrModuleTrees>>,
    modules: HashMap<String, usize>,
    /// Which library defines each linkable identity.
    owners: HashMap<(String, KlibIrSignature), usize>,
    /// The producer-declared module graph. These are manifest `depends` edges, not the roles the
    /// current compilation assigned its inputs.
    dependencies: HashMap<String, Vec<String>>,
}

/// A public classifier search over one producer dependency closure.
pub(crate) enum LocatedClass<'a> {
    Missing,
    One(&'a KlibIrClass),
    Several,
}

/// The class that directly declares one function, and the arena that owns both.
pub(crate) enum LocatedMember<'a> {
    Missing,
    Several,
    One {
        module: String,
        arena: &'a KlibIrArena,
        class: &'a KlibIrClass,
        function: &'a KlibIrFunction,
    },
}

/// The top-level property that declares one backing field, and the arena that owns both.
pub(crate) enum LocatedStaticField<'a> {
    Missing,
    Several,
    One {
        arena: &'a KlibIrArena,
        property: &'a crate::metadata::klib_ir::tree::KlibIrProperty,
        field: &'a crate::metadata::klib_ir::tree::KlibIrField,
    },
}

/// A top-level property whose backing field a file's initialization stores by running code.
pub(crate) struct StaticFieldInitializer<'a> {
    pub(crate) arena: &'a KlibIrArena,
    pub(crate) property: &'a crate::metadata::klib_ir::tree::KlibIrProperty,
    pub(crate) field: &'a crate::metadata::klib_ir::tree::KlibIrField,
    pub(crate) initializer: crate::metadata::klib_ir::tree::KlibIrExprId,
}

pub(crate) struct KlibClassMember<'a> {
    pub(crate) arena: &'a KlibIrArena,
    pub(crate) class: &'a crate::metadata::klib_ir::tree::KlibIrClass,
    pub(crate) function: &'a KlibIrFunction,
}

/// One decoded KLIB together with the producer graph recorded in its manifest.
pub struct KlibBodyLibrary {
    module: String,
    dependencies: Vec<String>,
    trees: std::sync::Arc<KlibIrModuleTrees>,
}

impl KlibBodyLibrary {
    /// Pair decoded trees with the identity and producer edges recorded by their archive.
    /// A manifest without `unique_name` cannot provide stable cross-archive identity.
    pub fn from_manifest(manifest: &KlibManifest, trees: KlibIrModuleTrees) -> Option<Self> {
        Some(Self::over(
            manifest.unique_name()?.to_owned(),
            manifest.depends().into_iter().map(str::to_owned),
            trees,
        ))
    }

    /// As [`Self::from_manifest`], over trees another compilation of the same archive decoded.
    pub(crate) fn from_manifest_shared(
        manifest: &KlibManifest,
        trees: std::sync::Arc<KlibIrModuleTrees>,
    ) -> Option<Self> {
        Some(Self {
            module: manifest.unique_name()?.to_owned(),
            dependencies: manifest.depends().into_iter().map(str::to_owned).collect(),
            trees,
        })
    }

    /// Add the platform stdlib `module` as a dependency of this library. Every KLIB is compiled
    /// and linked against its platform stdlib, which a producer need not record in `depends`:
    /// kotlinc's Wasm and JS libraries record no `depends` at all.
    pub(crate) fn depending_on_platform(mut self, module: &str) -> Self {
        if self.module != module && !self.dependencies.iter().any(|known| known == module) {
            self.dependencies.push(module.to_owned());
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn new(
        module: String,
        dependencies: impl IntoIterator<Item = String>,
        trees: KlibIrModuleTrees,
    ) -> Self {
        Self::over(module, dependencies, trees)
    }

    fn over(
        module: String,
        dependencies: impl IntoIterator<Item = String>,
        trees: KlibIrModuleTrees,
    ) -> Self {
        Self {
            module,
            dependencies: dependencies.into_iter().collect(),
            trees: std::sync::Arc::new(trees),
        }
    }
}

/// Exact origin of a serialized body call in the producer's dependency graph.
pub(crate) enum KlibCalleeOrigin {
    Exact(KlibDependencyCallable),
    Missing,
    Ambiguous,
}

/// Two library inputs claim one module identity, or duplicate one declaration inside it.
///
/// The signature is boxed so a conflict stays a small `Result` error. The signature is only
/// read when reporting the conflict; the success path never carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibDeclarationBodiesError {
    module: String,
    signature: Option<Box<KlibIrSignature>>,
    libraries: [usize; 2],
}

impl KlibDeclarationBodiesError {
    pub fn module(&self) -> &str {
        &self.module
    }

    /// The exact duplicated declaration, or `None` when two archives claim the module itself.
    pub fn signature(&self) -> Option<&KlibIrSignature> {
        self.signature.as_deref()
    }

    /// The positions, in input order, of the two conflicting libraries.
    pub fn libraries(&self) -> [usize; 2] {
        self.libraries
    }
}

impl std::fmt::Display for KlibDeclarationBodiesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [first, second] = self.libraries;
        match &self.signature {
            Some(signature) => write!(
                formatter,
                "KLIB libraries {first} and {second} both define the declaration {signature:?} in module {}",
                self.module
            ),
            None => write!(
                formatter,
                "KLIB libraries {first} and {second} both claim module {}",
                self.module
            ),
        }
    }
}

impl std::error::Error for KlibDeclarationBodiesError {}

/// What one emitted body reaches; see [`KlibDeclarationBodies::emitted_body`]. Declaration roots
/// never become executable call obligations merely by sharing this walk with callable edges.
pub(crate) struct EmittedBody {
    pub(crate) callables: Vec<KlibDependencyCallable>,
    pub(crate) objects: Vec<KlibPublicIdSignature>,
    pub(crate) sam_classifiers: Vec<(KlibPublicIdSignature, TypeName)>,
    pub(crate) hierarchy_roots: Vec<TypeName>,
}

/// Distinct inventories from one authenticated emitted-body traversal. Declaration roots never
/// become executable call obligations merely by sharing this walk with callable symbols.
#[derive(Default)]
struct EmittedBodyInventory<'a> {
    callables: Vec<&'a KlibIrSymbol>,
    objects: Vec<&'a KlibIrSymbol>,
    enum_entries: Vec<&'a KlibIrSymbol>,
    public_hierarchy_roots: Vec<TypeName>,
    sam_declarations: Vec<&'a KlibIrSymbol>,
}

impl KlibDeclarationBodies {
    /// Exactly one raw class header in this module with the requested function/constructor or
    /// accessor as a direct member. An accessor is found only in the slot of its `role`, or, for a
    /// private accessor no provider records a role for (`None`), in either slot. Either way a slot
    /// holds only an accessor of its own shape, a getter without value parameters and a setter
    /// with one, so a getter stored in a setter slot grants no authority. Equal names in other
    /// archives and inherited members do not join.
    pub(crate) fn class_member_header(
        &self,
        identity: &KlibDependencyCallable,
        signature: &KlibPublicIdSignature,
        role: Option<crate::libraries::KlibAccessorRole>,
    ) -> Result<Option<KlibClassMember<'_>>, &'static str> {
        let Some((arena, class)) = self.class_header(&identity.module, signature)? else {
            return Ok(None);
        };
        let target = ir_signature(&identity.declaration);
        let functions = class
            .members
            .iter()
            .flat_map(|member| match (&identity.declaration, member) {
                (KlibDeclarationSignature::Public(_), KlibIrMember::Function(id)) => {
                    [Some(*id), None]
                }
                (
                    KlibDeclarationSignature::Accessor(accessor),
                    KlibIrMember::Property(property),
                ) if property.base.symbol.kind == KlibIrSymbolKind::Property
                    && property.base.symbol.signature
                        == KlibIrSignature::Public(accessor.property().clone()) =>
                {
                    use crate::libraries::KlibAccessorRole::{Getter, Setter};
                    let slot = |accessor: Option<
                        crate::metadata::klib_ir::tree::KlibIrFunctionId,
                    >,
                                parameters: usize| {
                        accessor
                            .filter(|id| arena.function(*id).regular_parameters.len() == parameters)
                    };
                    [
                        slot(property.getter, 0).filter(|_| role != Some(Setter)),
                        slot(property.setter, 1).filter(|_| role != Some(Getter)),
                    ]
                }
                _ => [None, None],
            })
            .flatten()
            .filter(|id| arena.function(*id).base.symbol.signature == target)
            .collect::<Vec<_>>();
        let [function] = functions.as_slice() else {
            return Err("the serialized callable is not a unique direct class member");
        };
        Ok(Some(KlibClassMember {
            arena,
            class,
            function: arena.function(*function),
        }))
    }

    /// The serialized accessor `identity` names of a top-level property, found only in the `role`
    /// slot of the top-level property group its accessor signature names. As in
    /// [`Self::class_member_header`], a slot holds only an accessor of its own shape, so neither a
    /// function of the same signature elsewhere nor an accessor of another slot or property
    /// grants authority. `None` when the module serializes no function under the identity.
    pub(crate) fn top_level_accessor_header(
        &self,
        identity: &KlibDependencyCallable,
        role: crate::libraries::KlibAccessorRole,
    ) -> Result<Option<(&KlibIrArena, &KlibIrFunction)>, &'static str> {
        let KlibDeclarationSignature::Accessor(accessor) = &identity.declaration else {
            return Err("a top-level accessor is selected under a non-accessor signature");
        };
        let Some((trees, arena, function)) = self.function_with_trees(identity) else {
            return Ok(None);
        };
        let group = KlibIrSignature::Public(accessor.property().clone());
        let slots = trees
            .trees()
            .iter()
            .filter_map(|tree| match &tree.declaration {
                KlibIrMember::Property(property)
                    if property.base.symbol.kind == KlibIrSymbolKind::Property
                        && property.base.symbol.signature == group =>
                {
                    let (slot, parameters) = match role {
                        crate::libraries::KlibAccessorRole::Getter => (property.getter, 0),
                        crate::libraries::KlibAccessorRole::Setter => (property.setter, 1),
                    };
                    slot.map(|id| tree.arena.function(id))
                        .filter(|slot| slot.regular_parameters.len() == parameters)
                }
                _ => None,
            })
            .filter(|slot| std::ptr::eq(*slot, function))
            .count();
        if slots != 1 {
            return Err(
                "the serialized accessor is not the unique accessor of its role in its \
                 top-level property",
            );
        }
        Ok(Some((arena, function)))
    }

    /// The exact serialized class header, independently of any callable it declares.
    pub(crate) fn class_header(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Result<Option<(&KlibIrArena, &KlibIrClass)>, &'static str> {
        let Some(&library) = self.modules.get(module) else {
            return Ok(None);
        };
        let mut found = None;
        for class in self.libraries[library]
            .classes_with_signature(&KlibIrSignature::Public(signature.clone()))
        {
            if found.replace(class).is_some() {
                return Err("duplicate serialized class headers");
            }
        }
        Ok(found)
    }

    /// The class that declares the enum entry `entry` read by a body of `body_module`, resolved
    /// in that body's producer graph as [`Self::callee_origin`] resolves a call: a declaration in
    /// the body module wins, otherwise exactly one reachable module must declare it.
    pub(crate) fn enum_entry_class(
        &self,
        body_module: &str,
        entry: &KlibPublicIdSignature,
    ) -> Result<Option<&crate::metadata::klib_ir::tree::KlibIrClass>, &'static str> {
        let entry = KlibIrSignature::Public(entry.clone());
        if let Some(found) = self.module_enum_entry_class(body_module, &entry)? {
            return Ok(Some(found));
        }
        let mut owner = None;
        for module in self.body_provider_modules(body_module) {
            if module == body_module {
                continue;
            }
            if let Some(found) = self.module_enum_entry_class(&module, &entry)? {
                if owner.replace(found).is_some() {
                    return Err("several producer modules declare one enum entry");
                }
            }
        }
        Ok(owner)
    }

    fn module_enum_entry_class(
        &self,
        module: &str,
        entry: &KlibIrSignature,
    ) -> Result<Option<&crate::metadata::klib_ir::tree::KlibIrClass>, &'static str> {
        let Some(&library) = self.modules.get(module) else {
            return Ok(None);
        };
        let mut found = None;
        for (_, class) in self.libraries[library].classes_declaring_enum_entry(entry) {
            if found.replace(class).is_some() {
                return Err("several serialized classes declare one enum entry");
            }
        }
        Ok(found)
    }

    /// Index each named module's decoded trees. Every module occurs once; unrelated modules may
    /// legally publish equal internal signatures.
    #[cfg(test)]
    pub(crate) fn from_libraries(
        libraries: Vec<(String, KlibIrModuleTrees)>,
    ) -> Result<Self, KlibDeclarationBodiesError> {
        Self::from_body_libraries(
            libraries
                .into_iter()
                .map(|(module, trees)| KlibBodyLibrary::over(module, Vec::new(), trees))
                .collect(),
        )
    }

    /// Index decoded libraries with their producer-declared manifest dependency edges.
    ///
    /// An empty edge set stays empty: manifests which omit `depends` provide no authority to
    /// search every input for an equal bare signature.
    pub fn from_body_libraries(
        libraries: Vec<KlibBodyLibrary>,
    ) -> Result<Self, KlibDeclarationBodiesError> {
        let mut owners = HashMap::default();
        let mut modules = HashMap::default();
        let mut duplicate_module = None;
        let mut dependencies = HashMap::<String, HashSet<String>>::default();
        for (library, input) in libraries.iter().enumerate() {
            if let Some(previous) = modules.insert(input.module.clone(), library) {
                duplicate_module.get_or_insert((input.module.clone(), [previous, library]));
            }
            dependencies
                .entry(input.module.clone())
                .or_default()
                .extend(input.dependencies.iter().cloned());
            for signature in input.trees.function_signatures() {
                let identity = (input.module.clone(), signature.clone());
                if let Some(previous) = owners.insert(identity, library) {
                    return Err(KlibDeclarationBodiesError {
                        module: input.module.clone(),
                        signature: Some(Box::new(signature.clone())),
                        libraries: [previous, library],
                    });
                }
            }
        }
        if let Some((module, libraries)) = duplicate_module {
            return Err(KlibDeclarationBodiesError {
                module,
                signature: None,
                libraries,
            });
        }
        Ok(Self {
            libraries: libraries.into_iter().map(|input| input.trees).collect(),
            modules,
            owners,
            dependencies: dependencies
                .into_iter()
                .map(|(module, dependencies)| (module, dependencies.into_iter().collect()))
                .collect(),
        })
    }

    /// Whether exactly one producer-reachable module serializes `classifier` with no
    /// object-initialization work. The checked receiver separately proves that this exact classifier
    /// is a singleton. This body-side proof admits only an `Any`-delegating empty constructor,
    /// literal field initialization and an empty anonymous initializer; member functions do no
    /// initialization work. An absent, multiply owned,
    /// inherited, or executable initializer grants no authority.
    pub(crate) fn singleton_initialization_is_inert(
        &self,
        module: &str,
        classifier: TypeName,
    ) -> bool {
        let wanted = KlibPublicIdSignature::classifier(classifier);
        let mut found = None;
        for module in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            let mut in_module = None;
            for (arena, class) in self.libraries[library].classes_at_path(&wanted) {
                if class.base.symbol.kind != KlibIrSymbolKind::Class
                    || class_identity(&class.base.symbol.signature) != Some(classifier)
                {
                    continue;
                }
                let inert = class_is_inert(arena, class);
                if !inert {
                    return false;
                }
                in_module = Some(true);
            }
            if let Some(inert) = in_module {
                if found.replace(inert).is_some() {
                    return false;
                }
            }
        }
        found == Some(true)
    }

    /// Modules whose bodies the producer graph rooted at `module` can publish.
    ///
    /// This is the same manifest-edge closure used to resolve serialized calls. It deliberately
    /// ignores unrelated current-compilation inputs: those archives cannot contribute a callee to
    /// a body rooted in the selected platform stdlib.
    pub(crate) fn body_provider_modules(&self, module: &str) -> HashSet<String> {
        let mut pending = VecDeque::from([module.to_owned()]);
        let mut reachable = HashSet::default();
        while let Some(module) = pending.pop_front() {
            if !reachable.insert(module.clone()) {
                continue;
            }
            if let Some(dependencies) = self.dependencies.get(&module) {
                pending.extend(dependencies.iter().cloned());
            }
        }
        reachable
    }

    /// Exact callable references these bodies serialize, by the module serializing them. No
    /// inherited alias enters the declaration index or the narrower intrinsic-support call graph.
    pub(super) fn callable_references(&self) -> Vec<(String, KlibDeclarationSignature)> {
        let mut references = HashSet::default();
        for (module, &library) in &self.modules {
            for (arena, function) in self.libraries[library].all_functions() {
                for symbol in arena.callable_symbols(function) {
                    if let Some(signature) = declaration_signature(&symbol.signature) {
                        references.insert((module.clone(), signature));
                    }
                }
            }
        }
        references.into_iter().collect()
    }

    /// The declaration a call in an ordinary body links to: the serialized origin, or for a
    /// member no library serializes, the real declaration it inherits through its declared
    /// supertypes. A call never links to a declaration that is neither.
    pub(crate) fn linked_origin(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> KlibCalleeOrigin {
        match self.callee_origin(body_module, declaration) {
            KlibCalleeOrigin::Missing => self.inherited_origin(body_module, declaration),
            origin => origin,
        }
    }

    /// Resolve a call symbol in the graph of the KLIB body which serialized it.
    ///
    /// A definition in the body module wins. Otherwise exactly one reachable producer module
    /// must define the signature. Input order and equal declarations outside that graph have no
    /// bearing on the answer. A member no module serializes, a fake override, stays missing here;
    /// [`Self::linked_origin`] follows it to the declaration it inherits.
    pub(crate) fn callee_origin(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> KlibCalleeOrigin {
        let signature = ir_signature(declaration);
        if let KlibDeclarationSignature::FileLocal { .. } = declaration {
            // A file-private function is its own module's; no dependency edge reaches one.
            let declared = self.modules.get(body_module).is_some_and(|&library| {
                self.libraries[library]
                    .file_private_function(&signature)
                    .is_some()
            });
            return if declared {
                KlibCalleeOrigin::Exact(KlibDependencyCallable {
                    module: body_module.to_owned(),
                    declaration: declaration.clone(),
                })
            } else {
                KlibCalleeOrigin::Missing
            };
        }
        if self
            .owners
            .contains_key(&(body_module.to_owned(), signature.clone()))
        {
            return KlibCalleeOrigin::Exact(KlibDependencyCallable {
                module: body_module.to_owned(),
                declaration: declaration.clone(),
            });
        }

        let mut pending = self
            .dependencies
            .get(body_module)
            .into_iter()
            .flatten()
            .cloned()
            .collect::<VecDeque<_>>();
        let mut visited = HashSet::default();
        let mut owner = None;
        while let Some(module) = pending.pop_front() {
            if !visited.insert(module.clone()) {
                continue;
            }
            if self
                .owners
                .contains_key(&(module.clone(), signature.clone()))
                && owner.replace(module.clone()).is_some()
            {
                return KlibCalleeOrigin::Ambiguous;
            }
            if let Some(dependencies) = self.dependencies.get(&module) {
                pending.extend(dependencies.iter().cloned());
            }
        }
        match owner {
            Some(module) => KlibCalleeOrigin::Exact(KlibDependencyCallable {
                module,
                declaration: declaration.clone(),
            }),
            None => KlibCalleeOrigin::Missing,
        }
    }

    /// The function declared under exactly `signature`, with the arena that owns its body.
    pub fn function(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<(&KlibIrArena, &KlibIrFunction)> {
        self.function_with_trees(identity)
            .map(|(_, arena, function)| (arena, function))
    }

    /// [`Self::function`], with the decoded trees that serialize it. A file-local identity the
    /// body names is a slot of one of their files' signature tables, so it is resolved in those
    /// same trees and nowhere else.
    fn function_with_trees(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<(&KlibIrModuleTrees, &KlibIrArena, &KlibIrFunction)> {
        let signature = ir_signature(&identity.declaration);
        if let KlibDeclarationSignature::FileLocal { .. } = identity.declaration {
            let trees = &self.libraries[*self.modules.get(&identity.module)?];
            let (arena, function) = trees.file_private_function(&signature)?;
            return Some((trees, arena, function));
        }
        let trees = &self.libraries[*self
            .owners
            .get(&(identity.module.clone(), signature.clone()))?];
        let (arena, function) = trees.function(&signature)?;
        Some((trees, arena, function))
    }

    /// The file-local class `module` itself serializes under `signature`.
    ///
    /// A file-local identity is meaningful only in the library that wrote it. Producer
    /// dependencies are not searched: the same file and slot in another library are a different
    /// declaration.
    pub(crate) fn module_local_class<'a>(
        &'a self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> Option<(&'a KlibIrArena, &'a KlibIrClass)> {
        let library = *self.modules.get(module)?;
        self.libraries[library].local_class(signature)
    }

    /// The top-level class private to its file that `module` itself serializes under the
    /// file-local `signature`, with the package of that file.
    pub(crate) fn module_file_private_class<'a>(
        &'a self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> Option<(&'a [String], &'a KlibIrClass)> {
        let library = *self.modules.get(module)?;
        self.libraries[library].file_private_class(signature)
    }

    /// The package segments of the serialized file `file` of `module`.
    pub(crate) fn module_file_package(&self, module: &str, file: u32) -> Option<&[String]> {
        let library = *self.modules.get(module)?;
        self.libraries[library].file_package(file)
    }

    /// The class `module` itself serializes under `signature`, whichever kind of signature names
    /// it. `None` unless exactly one class has it.
    pub(crate) fn module_declared_class<'a>(
        &'a self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> Option<(&'a KlibIrArena, &'a KlibIrClass)> {
        let library = *self.modules.get(module)?;
        let mut found = None;
        for class in self.libraries[library].classes_with_signature(signature) {
            if found.replace(class).is_some() {
                return None;
            }
        }
        found.or_else(|| self.libraries[library].local_class(signature))
    }

    /// What the serialized file-local `signature` of `module` records beside its table slot.
    pub(crate) fn module_file_local_origin(
        &self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> Option<&crate::metadata::klib_ir::KlibFileLocalOrigin> {
        let library = *self.modules.get(module)?;
        self.libraries[library].file_local_origin(signature)
    }

    /// The public function `module` itself serializes under `signature`.
    pub(crate) fn module_public_function<'a>(
        &'a self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> Option<(&'a KlibIrArena, &'a KlibIrFunction)> {
        let library = *self.modules.get(module)?;
        self.libraries[library].function(signature)
    }

    /// The file-local function `module` itself serializes under `signature`.
    pub(crate) fn module_local_function<'a>(
        &'a self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> Option<(&'a KlibIrArena, &'a KlibIrFunction)> {
        let library = *self.modules.get(module)?;
        self.libraries[library].local_function(signature)
    }

    /// The top-level property whose accessor is exactly `identity`, with the arena that owns it,
    /// and whether that accessor is its setter. The property is the one the accessor's signature
    /// names, and the accessor sits in the slot of its shape: a getter without value parameters,
    /// a setter with one. A function signature matching an accessor slot of another property
    /// proves nothing.
    pub(crate) fn top_level_accessor_property(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Result<
        Option<(
            &KlibIrArena,
            &crate::metadata::klib_ir::tree::KlibIrProperty,
            bool,
        )>,
        &'static str,
    > {
        let KlibDeclarationSignature::Accessor(accessor) = &identity.declaration else {
            return Ok(None);
        };
        let signature = ir_signature(&identity.declaration);
        let Some(&library) = self
            .owners
            .get(&(identity.module.clone(), signature.clone()))
        else {
            return Ok(None);
        };
        let property_signature = KlibIrSignature::Public(accessor.property().clone());
        let mut found = None;
        for tree in self.libraries[library].trees() {
            let KlibIrMember::Property(property) = &tree.declaration else {
                continue;
            };
            if property.base.symbol.kind != KlibIrSymbolKind::Property
                || property.base.symbol.signature != property_signature
            {
                continue;
            }
            let slot = |accessor: Option<crate::metadata::klib_ir::tree::KlibIrFunctionId>,
                        parameters: usize| {
                accessor.is_some_and(|id| {
                    let function = tree.arena.function(id);
                    function.base.symbol.signature == signature
                        && function.regular_parameters.len() == parameters
                })
            };
            for setter in [false, true].into_iter().filter(|&setter| {
                if setter {
                    slot(property.setter, 1)
                } else {
                    slot(property.getter, 0)
                }
            }) {
                if found.replace((&tree.arena, property, setter)).is_some() {
                    return Err(
                        "the serialized accessor is not a unique top-level property member",
                    );
                }
            }
        }
        Ok(found)
    }

    /// Whether initializing the serialized file `file` of `module` runs no code: every top-level
    /// backing field of that file is initialized with a constant or not at all. A top-level
    /// backing field whose identity names no file leaves every file's initialization unproven.
    pub(crate) fn module_file_initialization_is_inert(&self, module: &str, file: u32) -> bool {
        let Some(&library) = self.modules.get(module) else {
            return false;
        };
        self.libraries[library].trees().iter().all(|tree| {
            let KlibIrMember::Property(property) = &tree.declaration else {
                return true;
            };
            let Some(backing) = &property.backing_field else {
                return true;
            };
            match backing.base.symbol.signature {
                KlibIrSignature::FileLocal { file: declared, .. } if declared != file => true,
                KlibIrSignature::FileLocal { .. } => {
                    backing.initializer.is_none_or(|initializer| {
                        matches!(tree.arena.expr(initializer).kind, KlibIrExprKind::Const(_))
                    })
                }
                KlibIrSignature::Public(_)
                | KlibIrSignature::Accessor(_)
                | KlibIrSignature::TypeParameter { .. } => false,
            }
        })
    }

    /// The serialized file of `identity` when it is a top-level function or a top-level property
    /// accessor of its library: the file whose initialization a call of it runs first.
    pub(crate) fn top_level_file(&self, identity: &KlibDependencyCallable) -> Option<u32> {
        let signature = ir_signature(&identity.declaration);
        let library = match identity.declaration {
            KlibDeclarationSignature::FileLocal { .. } => *self.modules.get(&identity.module)?,
            KlibDeclarationSignature::Public(_) | KlibDeclarationSignature::Accessor(_) => *self
                .owners
                .get(&(identity.module.clone(), signature.clone()))?,
        };
        self.libraries[library].top_level_file(&signature)
    }

    /// The top-level properties of the serialized file `file` of `module` whose backing field is
    /// initialized by running code, in declaration order: what initializing that file runs. With
    /// `constants`, those initialized with a constant are among them, in that same order.
    pub(crate) fn module_file_initializers<'a>(
        &'a self,
        module: &str,
        file: u32,
        constants: bool,
    ) -> Vec<StaticFieldInitializer<'a>> {
        let Some(&library) = self.modules.get(module) else {
            return Vec::new();
        };
        self.libraries[library]
            .trees_with_files()
            .filter(|&(declared, _)| declared == file)
            .filter_map(|(_, tree)| {
                let KlibIrMember::Property(property) = &tree.declaration else {
                    return None;
                };
                let field = property.backing_field.as_ref()?;
                let initializer = field.initializer?;
                (constants
                    || !matches!(tree.arena.expr(initializer).kind, KlibIrExprKind::Const(_)))
                .then_some(StaticFieldInitializer {
                    arena: &tree.arena,
                    property,
                    field,
                    initializer,
                })
            })
            .collect()
    }

    /// The top-level property `module` itself serializes whose backing field is `field`.
    ///
    /// A backing field is read only by bodies of its own library, so producer dependencies are
    /// not searched.
    pub(crate) fn module_static_field<'a>(
        &'a self,
        module: &str,
        field: &KlibIrSymbol,
    ) -> LocatedStaticField<'a> {
        let Some(&library) = self.modules.get(module) else {
            return LocatedStaticField::Missing;
        };
        let mut found = LocatedStaticField::Missing;
        for tree in self.libraries[library].trees() {
            let KlibIrMember::Property(property) = &tree.declaration else {
                continue;
            };
            let Some(backing) = property
                .backing_field
                .as_ref()
                .filter(|backing| backing.base.symbol == *field)
            else {
                continue;
            };
            if !matches!(found, LocatedStaticField::Missing) {
                return LocatedStaticField::Several;
            }
            found = LocatedStaticField::One {
                arena: &tree.arena,
                property,
                field: backing,
            };
        }
        found
    }

    /// Whether `identity` is private to its library: a file-local function, or a constructor or
    /// member of a class private to its enclosing class. No provider publishes it; the library's
    /// own bodies link it as a file-local declaration.
    pub(crate) fn is_library_private(&self, identity: &KlibDependencyCallable) -> bool {
        let signature = match &identity.declaration {
            KlibDeclarationSignature::FileLocal { .. } => return true,
            KlibDeclarationSignature::Public(signature) => {
                KlibIrSignature::Public(signature.clone())
            }
            KlibDeclarationSignature::Accessor(signature) => {
                KlibIrSignature::Accessor(signature.clone())
            }
        };
        self.module_local_function(&identity.module, &signature)
            .is_some()
    }

    /// The class in `module` that declares the file-local function `signature` directly.
    pub(crate) fn module_class_declaring<'a>(
        &'a self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> Option<(&'a KlibIrArena, &'a KlibIrClass)> {
        let library = *self.modules.get(module)?;
        let mut found = None;
        for (arena, class, _) in self.libraries[library].classes_declaring_function(signature) {
            if found.replace((arena, class)).is_some() {
                return None;
            }
        }
        found
    }

    /// Whether the one public class `signature` serialized for `module` declares an enum's
    /// synthetic `entries` body.
    pub(crate) fn declares_synthetic_enum_entries(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> bool {
        matches!(
            self.class_header(module, signature),
            Ok(Some((arena, class))) if class.declares_synthetic_enum_entries(arena)
        )
    }

    /// A public classifier serialized by `module` or a library its manifest depends on.
    pub(crate) fn public_class<'a>(
        &'a self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> LocatedClass<'a> {
        let wanted = KlibIrSignature::Public(signature.clone());
        let mut found = None;
        for owner in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&owner) else {
                continue;
            };
            for (_, class) in self.libraries[library].classes_with_signature(&wanted) {
                if found.replace(class).is_some() {
                    return LocatedClass::Several;
                }
            }
        }
        match found {
            Some(class) => LocatedClass::One(class),
            None => LocatedClass::Missing,
        }
    }

    /// The type parameter `index` of the public class `owner` serialized by `module` or a library
    /// its manifest depends on, with the arena that owns its bounds. `None` unless exactly one
    /// class has the signature and declares that many type parameters.
    pub(crate) fn public_class_type_parameter<'a>(
        &'a self,
        module: &str,
        owner: &KlibPublicIdSignature,
        index: u32,
    ) -> Option<(&'a KlibIrArena, &'a KlibIrTypeParameter)> {
        let wanted = KlibIrSignature::Public(owner.clone());
        let mut found = None;
        for module in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            for (arena, class) in self.libraries[library].classes_with_signature(&wanted) {
                if found.replace((arena, class)).is_some() {
                    return None;
                }
            }
        }
        let (arena, class) = found?;
        let parameter = class.type_parameters.get(usize::try_from(index).ok()?)?;
        Some((arena, parameter))
    }

    /// The constructors the public class `signature` serialized by `module` or a library its
    /// manifest depends on declares, in member order. `None` unless exactly one class has the
    /// signature, or when a constructor's symbol is not public.
    pub(crate) fn public_class_constructors(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Option<Vec<KlibPublicIdSignature>> {
        let wanted = KlibIrSignature::Public(signature.clone());
        let mut found = None;
        for owner in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&owner) else {
                continue;
            };
            for class in self.libraries[library].classes_with_signature(&wanted) {
                if found.replace(class).is_some() {
                    return None;
                }
            }
        }
        let (arena, class) = found?;
        class
            .members
            .iter()
            .filter_map(|member| match member {
                KlibIrMember::Function(function) => {
                    Some(arena.function(*function)).filter(|function| function.constructor)
                }
                _ => None,
            })
            .map(|constructor| match &constructor.base.symbol.signature {
                KlibIrSignature::Public(signature) => Some(signature.clone()),
                _ => None,
            })
            .collect()
    }

    /// The direct member functions, constructors excluded, of the one public class `signature`
    /// in `module`'s producer graph, with the module that serializes it. `None` unless exactly one
    /// class has the signature.
    pub(crate) fn public_class_functions(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Option<(String, &KlibIrArena, Vec<&KlibIrFunction>)> {
        let (owner, arena, class) = self.unique_public_class(module, signature)?;
        let functions = class
            .members
            .iter()
            .filter_map(|member| match member {
                KlibIrMember::Function(function) => {
                    Some(arena.function(*function)).filter(|function| !function.constructor)
                }
                _ => None,
            })
            .collect();
        Some((owner, arena, functions))
    }

    /// The getter of the direct member property `name` of the one public class `signature` in
    /// `module`'s producer graph, with the module that serializes it. `None` unless exactly one
    /// class has the signature and it declares exactly one such property with a getter.
    pub(crate) fn public_class_member_getter(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
        name: &str,
    ) -> Option<KlibDependencyCallable> {
        let (owner, arena, class) = self.unique_public_class(module, signature)?;
        let mut getters = class.members.iter().filter_map(|member| match member {
            KlibIrMember::Property(property) if property.name == name => Some(property.getter),
            _ => None,
        });
        let getter = getters.next()??;
        if getters.next().is_some() {
            return None;
        }
        let getter = arena.function(getter);
        if getter.dispatch_receiver.is_none() || getter.extension_receiver.is_some() {
            return None;
        }
        let KlibIrSignature::Accessor(accessor) = &getter.base.symbol.signature else {
            return None;
        };
        Some(KlibDependencyCallable {
            module: owner,
            declaration: KlibDeclarationSignature::Accessor(accessor.clone()),
        })
    }

    /// The one class serialized under `signature` in `module`'s producer graph, with the module
    /// that serializes it.
    fn unique_public_class(
        &self,
        module: &str,
        signature: &KlibPublicIdSignature,
    ) -> Option<(String, &KlibIrArena, &KlibIrClass)> {
        let wanted = KlibIrSignature::Public(signature.clone());
        let mut found = None;
        for owner in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&owner) else {
                continue;
            };
            for (arena, class) in self.libraries[library].classes_with_signature(&wanted) {
                if found.replace((owner.clone(), arena, class)).is_some() {
                    return None;
                }
            }
        }
        found
    }

    /// The class in `module`'s producer graph that directly declares `signature`.
    ///
    /// The match is the function's serialized symbol, including a property accessor. Two classes
    /// declaring one symbol are [`LocatedMember::Several`].
    pub(crate) fn function_owner<'a>(
        &'a self,
        module: &str,
        signature: &KlibIrSignature,
    ) -> LocatedMember<'a> {
        let mut found = None;
        for owner in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&owner) else {
                continue;
            };
            for (arena, class, function) in
                self.libraries[library].classes_declaring_function(signature)
            {
                if found.is_some() {
                    return LocatedMember::Several;
                }
                found = Some(LocatedMember::One {
                    module: owner.clone(),
                    arena,
                    class,
                    function,
                });
            }
        }
        found.unwrap_or(LocatedMember::Missing)
    }

    /// The class that directly declares the exact linked member `identity`, function or accessor.
    pub(crate) fn member_owner(&self, identity: &KlibDependencyCallable) -> LocatedMember<'_> {
        self.function_owner(&identity.module, &ir_signature(&identity.declaration))
    }

    /// Whether the exact serialized declaration intentionally carries an executable body.
    pub(crate) fn function_has_body(&self, identity: &KlibDependencyCallable) -> Option<bool> {
        self.function(identity)
            .map(|(_, function)| function.body.is_some())
    }

    /// Public classifiers present in the serialized type table of one exact body declaration.
    ///
    /// The arena can also contain nested declarations that share the body's storage. Including
    /// those types is intentional: common lowering may emit their bodies as part of the same
    /// dependency unit.
    pub(crate) fn type_classifiers_in_body(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Vec<TypeName> {
        let Some((arena, _)) = self.function(identity) else {
            return Vec::new();
        };
        let mut names = HashSet::default();
        arena
            .classifier_signatures_in_types()
            .filter_map(class_identity)
            .filter(|name| names.insert(*name))
            .collect()
    }

    /// Exact producer-resolved callees named directly by this function body. Missing and
    /// ambiguous origins stay absent here and are diagnosed by body lowering; consumers use this
    /// inventory only to prepare capabilities for declarations which the same lowering reaches.
    pub(crate) fn direct_callees(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Vec<KlibDependencyCallable> {
        let Some((arena, function)) = self.function(identity) else {
            return Vec::new();
        };
        arena
            .call_symbols_in_body(function)
            .filter_map(|symbol| declaration_signature(&symbol.signature))
            .filter_map(
                |declaration| match self.linked_origin(&identity.module, &declaration) {
                    KlibCalleeOrigin::Exact(callee) => Some(callee),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                },
            )
            .collect()
    }

    /// The public classes the file-local classes of these bodies directly name as supertypes, each
    /// once and in a stable order. Over the prepared-inline copies, these are the dependency
    /// classes an object literal or local class extends wherever a call expands its template.
    pub(crate) fn local_class_supertypes(&self) -> Vec<TypeName> {
        let mut supertypes = self
            .libraries
            .iter()
            .flat_map(|library| library.local_classes())
            .flat_map(|(arena, class)| public_supertypes(arena, class))
            .collect::<Vec<_>>();
        // Stable across processes: ordered by the serialized identity, not by interning order.
        supertypes.sort_by(|left, right| {
            (left.package().segments(), left.declaration().segments())
                .cmp(&(right.package().segments(), right.declaration().segments()))
        });
        supertypes.dedup();
        supertypes
            .into_iter()
            .filter_map(|signature| class_identity(&KlibIrSignature::Public(signature.clone())))
            .collect()
    }

    /// The serialized declarations the member function `identity` overrides. A materialized
    /// class carries each as an override edge its dispatch goes through, so the declarations are
    /// reached with the member even though its body never names them.
    pub(crate) fn overridden_callables(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Vec<KlibDependencyCallable> {
        let Some((_, function)) = self.function(identity) else {
            return Vec::new();
        };
        function
            .overridden
            .iter()
            .filter_map(|symbol| member_signature(&symbol.signature))
            .filter_map(|declaration| {
                self.overridden_declarations(&identity.module, &declaration)
                    .ok()
            })
            .flatten()
            .collect()
    }

    /// What the emitted body of `identity` reaches, from one traversal of its emitted roots.
    ///
    /// The callable edges are exact module-qualified edges of the bodies common lowering
    /// structurally emits, including reached closure implementations. The supplied operation
    /// policy does not grant intrinsic authority; original direct inventories remain the authority
    /// for that separate decision. The objects are the public objects whose values the body reads,
    /// through the closures `forms` lowers with it and the members of the file-local classes it
    /// constructs. The SAM classifiers are the public functional interfaces its SAM conversions
    /// name. The hierarchy roots are the public supertypes of the private classes it constructs:
    /// declaration facts, not executable callees, since a private constructor is intentionally
    /// absent from the public callable inventory even when its class is materialized with this
    /// body.
    ///
    /// `inherited` names the declaration a call of a fake override of a local class stands for,
    /// which lowering links in place of the signature the body serializes.
    pub(crate) fn emitted_body(
        &self,
        identity: &KlibDependencyCallable,
        forms: crate::metadata::klib_ir::tree::EmittedCallForms,
        inherited: &dyn Fn(&KlibIrSignature) -> Option<KlibIrSignature>,
    ) -> EmittedBody {
        let inventory = self.emitted_body_symbols(identity, forms);
        let callables = inventory
            .callables
            .into_iter()
            .filter_map(|symbol| {
                let resolved = inherited(&symbol.signature);
                declaration_signature(resolved.as_ref().unwrap_or(&symbol.signature))
            })
            .filter_map(
                |declaration| match self.linked_origin(&identity.module, &declaration) {
                    KlibCalleeOrigin::Exact(callee) => Some(callee),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                },
            )
            .collect();
        let mut seen = crate::fx_hash::HashSet::default();
        let mut objects: Vec<KlibPublicIdSignature> = inventory
            .objects
            .into_iter()
            .filter_map(|symbol| match &symbol.signature {
                KlibIrSignature::Public(signature) => Some(signature.clone()),
                _ => None,
            })
            .filter(|signature| seen.insert(signature.clone()))
            .collect();
        // The enum class of each entry the body reads is read with it, resolved in the body's
        // producer graph as the read itself is.
        for class in self.enum_entry_classes(&identity.module, inventory.enum_entries) {
            if seen.insert(class.clone()) {
                objects.push(class);
            }
        }
        let sam_classifiers = inventory
            .sam_declarations
            .into_iter()
            .filter_map(|symbol| match &symbol.signature {
                KlibIrSignature::Public(signature) => {
                    class_identity(&symbol.signature).map(|name| (signature.clone(), name))
                }
                _ => None,
            })
            .collect();
        EmittedBody {
            callables,
            objects,
            sam_classifiers,
            hierarchy_roots: inventory.public_hierarchy_roots,
        }
    }

    /// The objects of [`Self::emitted_body`].
    pub(crate) fn emitted_body_objects(
        &self,
        identity: &KlibDependencyCallable,
        forms: crate::metadata::klib_ir::tree::EmittedCallForms,
    ) -> Vec<KlibPublicIdSignature> {
        self.emitted_body(identity, forms, &|_| None).objects
    }

    /// The enum classes declaring `entries`, read by a body of `body_module`, without repeats. An
    /// entry no single producer declares names no class here; reading it declines in lowering.
    pub(crate) fn enum_entry_classes<'a>(
        &self,
        body_module: &str,
        entries: impl IntoIterator<Item = &'a KlibIrSymbol>,
    ) -> Vec<KlibPublicIdSignature> {
        let mut classes = Vec::new();
        for entry in entries {
            let KlibIrSignature::Public(entry) = &entry.signature else {
                continue;
            };
            let Ok(Some(declared)) = self.enum_entry_class(body_module, entry) else {
                continue;
            };
            if let KlibIrSignature::Public(class) = &declared.base.symbol.signature {
                if !classes.contains(class) {
                    classes.push(class.clone());
                }
            }
        }
        classes
    }

    /// Callable, object, SAM and hierarchy inventories cover exactly the same emitted roots: the
    /// selected body, its file initialization, and the local classes those roots construct or
    /// whose object values they read.
    fn emitted_body_symbols<'a>(
        &'a self,
        identity: &KlibDependencyCallable,
        forms: crate::metadata::klib_ir::tree::EmittedCallForms,
    ) -> EmittedBodyInventory<'a> {
        let Some((trees, arena, function)) = self.function_with_trees(identity) else {
            return EmittedBodyInventory::default();
        };
        let mut symbols = arena.emitted_callable_symbols(function, forms);
        let mut objects = arena.emitted_object_symbols(function, forms);
        let mut sams = arena.emitted_sam_classifiers(function, forms);
        let mut entries = arena.emitted_enum_entry_symbols(function, forms);
        // Constructor initialization uses these same serialized roots in
        // `constructor_initializer_callables`, even when its fields call no declaration.
        if function.constructor {
            if let Some((arena, class)) =
                self.module_class_declaring(&identity.module, &ir_signature(&identity.declaration))
            {
                sams.extend(
                    arena.emitted_sam_classifiers_from(arena.class_initializer_roots(class), forms),
                );
            }
        }
        let mut classifiers = Vec::new();
        if let Some(file) = self.top_level_file(identity) {
            for initializer in self.module_file_initializers(&identity.module, file, false) {
                let roots = vec![initializer.initializer];
                symbols.extend(
                    initializer
                        .arena
                        .emitted_callable_symbols_from(roots.clone(), forms),
                );
                objects.extend(
                    initializer
                        .arena
                        .emitted_object_symbols_from(roots.clone(), forms),
                );
                sams.extend(
                    initializer
                        .arena
                        .emitted_sam_classifiers_from(roots.clone(), forms),
                );
                entries.extend(
                    initializer
                        .arena
                        .emitted_enum_entry_symbols_from(roots, forms),
                );
            }
        }
        // A constructed local class, or a local object whose value a body reads, is materialized
        // with every member and its superclass delegation, in the arena that serializes that
        // class. Its object value is the unit's own, so it leaves the read objects.
        let mut classes = crate::fx_hash::HashSet::default();
        let mut read_objects = Vec::new();
        let (mut next_symbol, mut next_object) = (0, 0);
        loop {
            let local = if let Some(symbol) = symbols.get(next_symbol).copied() {
                next_symbol += 1;
                constructed_local_class(trees, symbol)
            } else if let Some(symbol) = objects.get(next_object).copied() {
                next_object += 1;
                let local = read_local_object(trees, symbol);
                if local.is_none() {
                    read_objects.push(symbol);
                }
                local
            } else {
                break;
            };
            let Some((class_arena, class)) = local else {
                continue;
            };
            if classes.insert(&class.base.symbol) {
                classifiers.extend(
                    public_supertypes(class_arena, class).filter_map(|signature| {
                        class_identity(&KlibIrSignature::Public(signature.clone()))
                    }),
                );
                let mut roots = class_arena.class_member_roots(class);
                roots.extend(class_arena.class_superclass_delegations(class));
                symbols.extend(class_arena.emitted_callable_symbols_from(roots.clone(), forms));
                objects.extend(class_arena.emitted_object_symbols_from(roots.clone(), forms));
                sams.extend(class_arena.emitted_sam_classifiers_from(roots.clone(), forms));
                entries.extend(class_arena.emitted_enum_entry_symbols_from(roots, forms));
            }
        }
        EmittedBodyInventory {
            callables: symbols,
            objects: read_objects,
            enum_entries: entries,
            public_hierarchy_roots: classifiers,
            sam_declarations: sams,
        }
    }

    /// Declaration facts of the class whose exact reached constructor is `identity`. Field and
    /// anonymous initializers live beside that constructor, not in its function body. Reuse the
    /// same structural initializer walker as local-class preparation; this inventory freezes facts
    /// only and must not replace the initialization plan's executable root selection.
    pub(crate) fn constructor_initializer_callables(
        &self,
        identity: &KlibDependencyCallable,
        forms: crate::metadata::klib_ir::tree::EmittedCallForms,
    ) -> Vec<KlibDependencyCallable> {
        let Some((_, function)) = self.function(identity) else {
            return Vec::new();
        };
        if !function.constructor {
            return Vec::new();
        }
        let Some((arena, class)) =
            self.module_class_declaring(&identity.module, &ir_signature(&identity.declaration))
        else {
            return Vec::new();
        };
        arena
            .emitted_callable_symbols_from(arena.class_initializer_roots(class), forms)
            .into_iter()
            .filter_map(|symbol| declaration_signature(&symbol.signature))
            .filter_map(
                |declaration| match self.linked_origin(&identity.module, &declaration) {
                    KlibCalleeOrigin::Exact(callee) => Some(callee),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                },
            )
            .collect()
    }

    /// Whether `identity` is an executable dependency-support root: its own serialized body has a
    /// direct call edge to an intrinsic declaration authenticated from these same selected inputs,
    /// or a direct call edge to one such declaration.
    ///
    /// The single wrapper layer admits a target stdlib's semantic wrapper around its instruction
    /// wrapper, while deliberately not turning every transitive caller into support code. The
    /// eligibility check does not recurse beyond that one edge: an ordinary caller of the semantic
    /// wrapper is not a support root. Producer dependency edges resolve each call before the
    /// intrinsic table is consulted, so an unrelated equal signature cannot grant the capability
    /// and lowering never tries one body before falling back to another realization.
    pub(crate) fn is_supported_intrinsic_root(
        &self,
        identity: &KlibDependencyCallable,
        intrinsics: &super::KlibIntrinsicFacts,
    ) -> bool {
        if self.directly_calls_authenticated_intrinsic(identity, intrinsics) {
            return true;
        }
        self.direct_callees(identity)
            .iter()
            .any(|callee| self.directly_calls_authenticated_intrinsic(callee, intrinsics))
    }

    fn directly_calls_authenticated_intrinsic(
        &self,
        identity: &KlibDependencyCallable,
        intrinsics: &super::KlibIntrinsicFacts,
    ) -> bool {
        let Some((arena, function)) = self.function(identity) else {
            return false;
        };
        arena.call_symbols_in_body(function).any(|symbol| {
            let Some(declaration) = declaration_signature(&symbol.signature) else {
                return false;
            };
            let KlibCalleeOrigin::Exact(callee) =
                self.linked_origin(&identity.module, &declaration)
            else {
                return false;
            };
            intrinsics.get(&callee).is_some_and(|callable| {
                matches!(
                    callable.operation(),
                    crate::libraries::CompilerIntrinsic::ReinterpretBits
                        | crate::libraries::CompilerIntrinsic::ScalarInstruction(_)
                )
            })
        })
    }
}

fn class_is_inert(arena: &KlibIrArena, class: &KlibIrClass) -> bool {
    let mut constructors = class.members.iter().filter_map(|member| match member {
        KlibIrMember::Function(function) if arena.function(*function).constructor => {
            Some(arena.function(*function))
        }
        _ => None,
    });
    class.supertypes.len() == 1
        && inert_any_supertype(arena, class.supertypes[0])
        && constructors
            .next()
            .is_some_and(|constructor| inert_constructor(arena, constructor, &class.base.symbol))
        && constructors.next().is_none()
        && class.members.iter().all(|member| match member {
            KlibIrMember::AnonymousInitializer { statements, .. } => statements.is_empty(),
            KlibIrMember::Field(field) => field
                .initializer
                .is_none_or(|initializer| inert_initializer(arena, initializer)),
            KlibIrMember::Property(property) => {
                property.backing_field.as_ref().is_none_or(|field| {
                    field
                        .initializer
                        .is_none_or(|initializer| inert_initializer(arena, initializer))
                })
            }
            // A member function, the constructor aside, runs only when it is called.
            _ => true,
        })
}

fn inert_any_supertype(
    arena: &KlibIrArena,
    ty: crate::metadata::klib_ir::tree::KlibIrTypeId,
) -> bool {
    matches!(
        arena.ty(ty),
        KlibIrType::Simple {
            classifier,
            nullability: crate::metadata::klib_ir::tree::KlibIrNullability::DefinitelyNotNull,
            arguments,
            abbreviation: None,
            ..
        } if arguments.is_empty()
            && classifier.kind == KlibIrSymbolKind::Class
            && class_identity(&classifier.signature) == Some(crate::types::wk::any())
    )
}

fn inert_constructor(
    arena: &KlibIrArena,
    constructor: &KlibIrFunction,
    class: &crate::metadata::klib_ir::KlibIrSymbol,
) -> bool {
    if !constructor.type_parameters.is_empty()
        || constructor.dispatch_receiver.is_some()
        || !constructor.context_parameters.is_empty()
        || constructor.extension_receiver.is_some()
        || !constructor.regular_parameters.is_empty()
    {
        return false;
    }
    let Some(KlibIrBody::Block(statements)) = constructor.body.as_ref() else {
        return false;
    };
    let [KlibIrStatement::Expression(delegate), KlibIrStatement::Expression(initialize)] =
        statements.as_slice()
    else {
        return false;
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &arena.expr(*delegate).kind else {
        return false;
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || declaration_owner(&access.symbol.signature) != Some(crate::types::wk::any())
        || !arguments_are_absent(&access.arguments)
    {
        return false;
    }
    matches!(
        &arena.expr(*initialize).kind,
        KlibIrExprKind::InstanceInitializerCall(initialized) if initialized == class
    )
}

fn arguments_are_absent(arguments: &crate::metadata::klib_ir::tree::KlibIrArguments) -> bool {
    match arguments {
        crate::metadata::klib_ir::tree::KlibIrArguments::Flat(arguments) => {
            arguments.iter().all(Option::is_none)
        }
        crate::metadata::klib_ir::tree::KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => {
            dispatch_receiver.is_none()
                && extension_receiver.is_none()
                && values.iter().all(Option::is_none)
        }
    }
}

fn inert_initializer(
    arena: &KlibIrArena,
    initializer: crate::metadata::klib_ir::tree::KlibIrExprId,
) -> bool {
    matches!(
        arena.expr(initializer).kind,
        crate::metadata::klib_ir::tree::KlibIrExprKind::Const(_)
    )
}

fn public_supertypes<'a>(
    arena: &'a KlibIrArena,
    class: &'a KlibIrClass,
) -> impl Iterator<Item = &'a KlibPublicIdSignature> {
    class.supertypes.iter().filter_map(|&supertype| {
        let KlibIrType::Simple { classifier, .. } = arena.ty(supertype) else {
            return None;
        };
        match &classifier.signature {
            KlibIrSignature::Public(signature) if classifier.kind == KlibIrSymbolKind::Class => {
                Some(signature)
            }
            _ => None,
        }
    })
}

fn class_identity(signature: &KlibIrSignature) -> Option<TypeName> {
    let KlibIrSignature::Public(signature) = signature else {
        return None;
    };
    if signature.member_id().is_some() || signature.declaration().is_empty() {
        return None;
    }
    let valid = |segment: &String| !segment.is_empty() && !segment.contains('/');
    if !signature.package().segments().iter().all(valid)
        || !signature.declaration().segments().iter().all(valid)
    {
        return None;
    }
    let mut identity = signature
        .package()
        .segments()
        .iter()
        .fold(TypeName::ROOT, |parent, segment| {
            type_name_child(parent, segment)
        });
    let mut declarations = signature.declaration().segments().iter();
    identity = type_name_child(identity, declarations.next()?);
    for nested in declarations {
        identity = identity.nested_child(nested);
    }
    Some(identity)
}

fn declaration_owner(signature: &KlibIrSignature) -> Option<TypeName> {
    let KlibIrSignature::Public(signature) = signature else {
        return None;
    };
    let (_, declaration) = signature.declaration().segments().split_last()?;
    if declaration.is_empty() {
        return None;
    }
    let valid = |segment: &String| !segment.is_empty() && !segment.contains('/');
    if !signature.package().segments().iter().all(valid) || !declaration.iter().all(valid) {
        return None;
    }
    let mut identity = signature
        .package()
        .segments()
        .iter()
        .fold(TypeName::ROOT, |parent, segment| {
            type_name_child(parent, segment)
        });
    let mut declarations = declaration.iter();
    identity = type_name_child(identity, declarations.next()?);
    for nested in declarations {
        identity = identity.nested_child(nested);
    }
    Some(identity)
}

pub(super) fn ir_signature(declaration: &KlibDeclarationSignature) -> KlibIrSignature {
    match declaration {
        KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature.clone()),
        KlibDeclarationSignature::Accessor(signature) => {
            KlibIrSignature::Accessor(signature.clone())
        }
        &KlibDeclarationSignature::FileLocal { file, slot } => {
            KlibIrSignature::FileLocal { file, slot }
        }
    }
}

/// The library-private class whose constructor `symbol` names, resolved in `trees`, the decoded trees
/// whose signature tables numbered the symbol, with the arena that serializes the class and its
/// constructor.
fn constructed_local_class<'a>(
    trees: &'a KlibIrModuleTrees,
    symbol: &KlibIrSymbol,
) -> Option<(&'a KlibIrArena, &'a KlibIrClass)> {
    // `local_function` answers a file-local constructor and one of a class private to its
    // enclosing class; a published constructor is not there.
    if symbol.kind != KlibIrSymbolKind::Constructor {
        return None;
    }
    let (function_arena, function) = trees.local_function(&symbol.signature)?;
    let KlibIrType::Simple { classifier, .. } = function_arena.ty(function.return_type) else {
        return None;
    };
    let (class_arena, class) = trees.local_class(&classifier.signature)?;
    (function.constructor && std::ptr::eq(function_arena, class_arena))
        .then_some((class_arena, class))
}

/// The library-private object whose value `symbol` reads, resolved in `trees`, with the arena
/// that serializes it.
fn read_local_object<'a>(
    trees: &'a KlibIrModuleTrees,
    symbol: &KlibIrSymbol,
) -> Option<(&'a KlibIrArena, &'a KlibIrClass)> {
    if symbol.kind != KlibIrSymbolKind::Class {
        return None;
    }
    trees.local_class(&symbol.signature).filter(|(_, class)| {
        crate::metadata::semantic::class_kind(class.base.flags)
            == crate::libraries::TypeKind::Object
    })
}

fn declaration_signature(signature: &KlibIrSignature) -> Option<KlibDeclarationSignature> {
    match signature {
        KlibIrSignature::Public(signature) => {
            Some(KlibDeclarationSignature::Public(signature.clone()))
        }
        KlibIrSignature::Accessor(signature) => {
            Some(KlibDeclarationSignature::Accessor(signature.clone()))
        }
        &KlibIrSignature::FileLocal { file, slot } => {
            Some(KlibDeclarationSignature::FileLocal { file, slot })
        }
        KlibIrSignature::TypeParameter { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::id_signature::{
        callable_signature, class_signature, CallableShape, ClassScope, DeclarationContainer,
        Placement,
    };
    use crate::metadata::klib_ir::tree::{
        KlibIrArguments, KlibIrDeclarationBase, KlibIrMemberAccess, KlibIrNullability,
    };
    use crate::metadata::klib_ir::KlibIrSymbol;
    use crate::metadata::semantic::KotlinType;

    fn class_symbol(package: &str, name: &str) -> KlibIrSymbol {
        let package = [package.to_owned()];
        let class = ClassScope::<KotlinType> {
            name,
            type_parameters: Vec::new(),
            expect: false,
        };
        KlibIrSymbol {
            kind: KlibIrSymbolKind::Class,
            signature: KlibIrSignature::Public(class_signature(
                DeclarationContainer {
                    package: &package,
                    classes: &[],
                    native_interop_library: false,
                },
                &class,
            )),
        }
    }

    fn constructor_symbol(package: &str, owner: &str) -> KlibIrSymbol {
        let package = [package.to_owned()];
        let classes = [ClassScope::<KotlinType> {
            name: owner,
            type_parameters: Vec::new(),
            expect: false,
        }];
        let signature = callable_signature(
            DeclarationContainer {
                package: &package,
                classes: &classes,
                native_interop_library: false,
            },
            &CallableShape {
                name: "<init>",
                contexts: Vec::new(),
                receiver: None,
                params: Vec::new(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("an empty constructor is signable");
        KlibIrSymbol {
            kind: KlibIrSymbolKind::Constructor,
            signature: KlibIrSignature::Public(signature),
        }
    }

    fn empty_constructor(
        arena: &mut KlibIrArena,
        class: KlibIrSymbol,
        delegate: KlibIrSymbol,
    ) -> KlibIrFunction {
        let ty = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let delegate = arena.push_expr(
            None,
            KlibIrExprKind::DelegatingConstructorCall(KlibIrMemberAccess {
                symbol: delegate,
                arguments: KlibIrArguments::Flat(Vec::new()),
                type_arguments: Vec::new(),
                origin: None,
            }),
        );
        let initialize = arena.push_expr(None, KlibIrExprKind::InstanceInitializerCall(class));
        KlibIrFunction {
            base: KlibIrDeclarationBase {
                symbol: KlibIrSymbol {
                    kind: KlibIrSymbolKind::Constructor,
                    signature: KlibIrSignature::FileLocal { file: 0, slot: 0 },
                },
                origin: "DEFINED".to_owned(),
                flags: 0,
                annotations: Vec::new(),
            },
            name: "<init>".to_owned(),
            constructor: true,
            type_parameters: Vec::new(),
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver: None,
            regular_parameters: Vec::new(),
            return_type: ty,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: None,
            body: Some(KlibIrBody::Block(vec![
                KlibIrStatement::Expression(delegate),
                KlibIrStatement::Expression(initialize),
            ])),
        }
    }

    /// A module serializing one top-level property `group` whose single accessor, signed as the
    /// getter of `signed_group`, stands in the `setter` slot when `in_setter_slot`.
    fn top_level_property(
        group: &KlibPublicIdSignature,
        signed_group: &KlibPublicIdSignature,
        in_setter_slot: bool,
    ) -> (KlibDeclarationBodies, KlibDependencyCallable) {
        let accessor = crate::metadata::id_signature::KlibAccessorIdSignature::new(
            signed_group.clone(),
            "<get-lastIndex>".to_owned(),
            1,
            0,
        );
        let mut arena = KlibIrArena::default();
        let mut getter = empty_constructor(
            &mut arena,
            class_symbol("fixture", "Unused"),
            constructor_symbol("kotlin", "Any"),
        );
        getter.base.symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::Function,
            signature: KlibIrSignature::Accessor(accessor.clone()),
        };
        getter.name = "<get-lastIndex>".to_owned();
        getter.constructor = false;
        getter.body = Some(KlibIrBody::Block(Vec::new()));
        let getter = arena.push_function(getter);
        let property = KlibIrMember::Property(crate::metadata::klib_ir::tree::KlibIrProperty {
            base: KlibIrDeclarationBase {
                symbol: KlibIrSymbol {
                    kind: KlibIrSymbolKind::Property,
                    signature: KlibIrSignature::Public(group.clone()),
                },
                origin: "DEFINED".to_owned(),
                flags: 0,
                annotations: Vec::new(),
            },
            name: "lastIndex".to_owned(),
            backing_field: None,
            getter: (!in_setter_slot).then_some(getter),
            setter: in_setter_slot.then_some(getter),
        });
        let trees =
            KlibIrModuleTrees::from_trees(vec![crate::metadata::klib_ir::KlibIrDeclarationTree {
                arena,
                declaration: property,
            }])
            .expect("the fixture decodes");
        let bodies = KlibDeclarationBodies::from_libraries(vec![("fixture".to_owned(), trees)])
            .expect("the fixture has one module");
        let identity = KlibDependencyCallable {
            module: "fixture".to_owned(),
            declaration: KlibDeclarationSignature::Accessor(accessor),
        };
        (bodies, identity)
    }

    #[test]
    fn a_top_level_accessor_is_authenticated_by_its_property_group_and_role_slot() {
        use crate::libraries::KlibAccessorRole::{Getter, Setter};
        let KlibIrSignature::Public(group) = class_symbol("fixture", "LastIndex").signature else {
            unreachable!()
        };
        let KlibIrSignature::Public(other) = class_symbol("fixture", "Other").signature else {
            unreachable!()
        };
        let (bodies, identity) = top_level_property(&group, &group, false);
        let (_, getter) = bodies
            .top_level_accessor_header(&identity, Getter)
            .expect("the getter holds its slot")
            .expect("the module serializes the getter");
        assert!(std::ptr::eq(getter, bodies.function(&identity).unwrap().1));
        let refused = Err(
            "the serialized accessor is not the unique accessor of its role in its top-level \
             property",
        );
        assert_eq!(
            bodies
                .top_level_accessor_header(&identity, Setter)
                .map(|found| found.is_some()),
            refused
        );
        // An accessor signed for another property group grants no authority over this one.
        let (bodies, identity) = top_level_property(&group, &other, false);
        assert_eq!(
            bodies
                .top_level_accessor_header(&identity, Getter)
                .map(|found| found.is_some()),
            refused
        );
        // A getter-shaped function in the setter slot is neither the getter nor the setter.
        let (bodies, identity) = top_level_property(&group, &group, true);
        for role in [Getter, Setter] {
            assert_eq!(
                bodies
                    .top_level_accessor_header(&identity, role)
                    .map(|found| found.is_some()),
                refused
            );
        }
        let missing = KlibDependencyCallable {
            module: "fixture".to_owned(),
            declaration: KlibDeclarationSignature::Accessor(
                crate::metadata::id_signature::KlibAccessorIdSignature::new(
                    group,
                    "<get-other>".to_owned(),
                    2,
                    0,
                ),
            ),
        };
        assert_eq!(
            bodies
                .top_level_accessor_header(&missing, Getter)
                .map(|found| found.is_some()),
            Ok(false)
        );
    }

    #[test]
    fn body_provider_modules_follow_only_the_producer_manifest_closure() {
        let bodies = KlibDeclarationBodies::from_body_libraries(vec![
            KlibBodyLibrary::new(
                "stdlib".to_owned(),
                ["middle".to_owned()],
                KlibIrModuleTrees::default(),
            ),
            KlibBodyLibrary::new(
                "middle".to_owned(),
                ["leaf".to_owned()],
                KlibIrModuleTrees::default(),
            ),
            KlibBodyLibrary::new(
                "leaf".to_owned(),
                Vec::<String>::new(),
                KlibIrModuleTrees::default(),
            ),
            KlibBodyLibrary::new(
                "unrelated".to_owned(),
                Vec::<String>::new(),
                KlibIrModuleTrees::default(),
            ),
        ])
        .expect("the fixture has unique modules");

        let reachable = bodies.body_provider_modules("stdlib");
        assert_eq!(reachable.len(), 3);
        assert!(reachable.contains("stdlib"));
        assert!(reachable.contains("middle"));
        assert!(reachable.contains("leaf"));
        assert!(!reachable.contains("unrelated"));
    }

    #[test]
    fn inert_singleton_proof_rejects_superclass_and_constructor_effects() {
        let mut arena = KlibIrArena::default();
        let any = class_symbol("kotlin", "Any");
        let side_effect_base = class_symbol("fixture", "SideEffectBase");
        let class = class_symbol("fixture", "PlatformCompanion");
        let any_type = arena.push_type(KlibIrType::Simple {
            classifier: any.clone(),
            nullability: KlibIrNullability::DefinitelyNotNull,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        let base_type = arena.push_type(KlibIrType::Simple {
            classifier: side_effect_base,
            nullability: KlibIrNullability::DefinitelyNotNull,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        assert!(inert_any_supertype(&arena, any_type));
        assert!(!inert_any_supertype(&arena, base_type));

        let constructor = empty_constructor(
            &mut arena,
            class.clone(),
            constructor_symbol("kotlin", "Any"),
        );
        assert!(inert_constructor(&arena, &constructor, &class));

        let inherited = empty_constructor(
            &mut arena,
            class.clone(),
            constructor_symbol("fixture", "SideEffectBase"),
        );
        assert!(!inert_constructor(&arena, &inherited, &class));

        let mut executable = empty_constructor(
            &mut arena,
            class.clone(),
            constructor_symbol("kotlin", "Any"),
        );
        let effect = arena.push_expr(None, KlibIrExprKind::GetObject(class.clone()));
        let Some(KlibIrBody::Block(statements)) = executable.body.as_mut() else {
            unreachable!()
        };
        statements.push(KlibIrStatement::Expression(effect));
        assert!(!inert_constructor(&arena, &executable, &class));
    }
}
