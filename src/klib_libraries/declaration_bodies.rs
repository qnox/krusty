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
    KlibIrArena, KlibIrBody, KlibIrExprKind, KlibIrFunction, KlibIrMember, KlibIrStatement,
    KlibIrType,
};
use crate::metadata::klib_ir::{
    KlibIrModuleTrees, KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind,
};
use crate::types::{type_name_child, TypeName};

/// The decoded function declarations of a set of KLIB libraries, joined by exact signature.
pub struct KlibDeclarationBodies {
    libraries: Vec<KlibIrModuleTrees>,
    modules: HashMap<String, usize>,
    /// Which library defines each linkable identity.
    owners: HashMap<(String, KlibIrSignature), usize>,
    /// The producer-declared module graph. These are manifest `depends` edges, not the roles the
    /// current compilation assigned its inputs.
    dependencies: HashMap<String, Vec<String>>,
}

/// What a search for one serialized enum entry found in a producer graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProvenEnumEntry {
    /// Exactly one class publishes the entry.
    One { classifier: TypeName, name: String },
    /// No class publishes it.
    None,
    /// More than one class publishes it, or its class has no public identity.
    Several,
}

/// A dependency enum whose constants the shared realization can build.
///
/// The proof is the serialized class: it extends `kotlin.Enum`, and every entry is a
/// declaration-order constant with no body and no constructor argument. Construction contains only
/// Enum delegation and own instance initialization; no additional storage or executable members
/// are discarded. A second class, a
/// file-private class, or an entry the common realization does not construct is not a simple
/// enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProvenEnumClass {
    /// One class, with its entries in serialized declaration order.
    Simple {
        identity: KlibDependencyCallable,
        entries: Vec<String>,
    },
    /// No loaded library publishes the class.
    None,
    /// More than one library publishes it.
    Several,
    /// The class is an enum the common realization does not construct as written.
    Unsupported(&'static str),
}

pub(crate) struct KlibClassMember<'a> {
    pub(crate) arena: &'a KlibIrArena,
    pub(crate) class: &'a crate::metadata::klib_ir::tree::KlibIrClass,
    pub(crate) function: &'a KlibIrFunction,
}

impl Default for KlibDeclarationBodies {
    fn default() -> Self {
        Self {
            libraries: Vec::new(),
            modules: HashMap::default(),
            owners: HashMap::default(),
            dependencies: HashMap::default(),
        }
    }
}

/// One decoded KLIB together with the producer graph recorded in its manifest.
pub struct KlibBodyLibrary {
    module: String,
    dependencies: Vec<String>,
    trees: KlibIrModuleTrees,
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
            trees,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibDeclarationBodiesError {
    module: String,
    signature: Option<KlibIrSignature>,
    libraries: [usize; 2],
}

impl KlibDeclarationBodiesError {
    pub fn module(&self) -> &str {
        &self.module
    }

    /// The exact duplicated declaration, or `None` when two archives claim the module itself.
    pub fn signature(&self) -> Option<&KlibIrSignature> {
        self.signature.as_ref()
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

impl KlibDeclarationBodies {
    /// Exactly one raw class header in this module with the requested function/constructor/getter
    /// as a direct member. The current property primitives are reads: a getter stored in a setter
    /// slot grants no authority. Equal names in other archives and inherited members do not join.
    pub(crate) fn class_member_header(
        &self,
        identity: &KlibDependencyCallable,
        signature: &KlibPublicIdSignature,
    ) -> Result<Option<KlibClassMember<'_>>, &'static str> {
        let Some(&library) = self.modules.get(&identity.module) else {
            return Ok(None);
        };
        let mut found = None;
        for tree in self.libraries[library].trees() {
            let mut pending = vec![&tree.declaration];
            while let Some(member) = pending.pop() {
                let class = match member {
                    KlibIrMember::Class(id) => tree.arena.class(*id),
                    KlibIrMember::EnumEntry(entry) => {
                        let Some(id) = entry.class else { continue };
                        tree.arena.class(id)
                    }
                    _ => continue,
                };
                pending.extend(class.members.iter());
                if class.base.symbol.signature != KlibIrSignature::Public(signature.clone()) {
                    continue;
                }
                if found.is_some() {
                    return Err("duplicate serialized class headers");
                }
                found = Some((&tree.arena, class));
            }
        }
        let Some((arena, class)) = found else {
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
                    [property.getter, None]
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
                        signature: Some(signature.clone()),
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
        let mut found = None;
        for module in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            let mut in_module = None;
            for tree in self.libraries[library].trees() {
                find_class(&tree.arena, &tree.declaration, classifier, &mut in_module);
                if in_module == Some(false) {
                    return false;
                }
            }
            if let Some(inert) = in_module {
                if found.replace(inert).is_some() {
                    return false;
                }
            }
        }
        found == Some(true)
    }

    /// The one enum entry in the producer graph rooted at `module` whose symbol is `symbol`.
    ///
    /// The entry is a serialized `EnumEntry` member of exactly one class. A second class, or an
    /// entry whose enclosing class has no public identity, is [`ProvenEnumEntry::Several`]: the
    /// caller declines rather than picking one.
    pub(crate) fn proven_enum_entry(&self, module: &str, symbol: &KlibIrSymbol) -> ProvenEnumEntry {
        let mut found = None;
        let mut several = false;
        for module in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            for tree in self.libraries[library].trees() {
                find_enum_entry(
                    &tree.arena,
                    &tree.declaration,
                    symbol,
                    None,
                    &mut found,
                    &mut several,
                );
            }
        }
        match (found, several) {
            (Some((classifier, name)), false) => ProvenEnumEntry::One { classifier, name },
            (_, true) => ProvenEnumEntry::Several,
            (None, false) => ProvenEnumEntry::None,
        }
    }

    /// The one simple enum class `classifier` in the producer graph rooted at `module`.
    ///
    /// Entry order is the serialized member order, which is Kotlin's declaration order and
    /// therefore each constant's ordinal. A class that is not this shape is
    /// [`ProvenEnumClass::Unsupported`] or [`ProvenEnumClass::None`]; the caller declines it
    /// rather than inventing a constant.
    pub(crate) fn proven_simple_enum(&self, module: &str, classifier: TypeName) -> ProvenEnumClass {
        let mut found: Option<(String, Result<SimpleEnumClass, &'static str>)> = None;
        for module in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            for tree in self.libraries[library].trees() {
                let Some(class) = find_simple_enum(&tree.arena, &tree.declaration, classifier)
                else {
                    continue;
                };
                if found.is_some() {
                    return ProvenEnumClass::Several;
                }
                found = Some((module.clone(), class));
            }
        }
        let Some((module, class)) = found else {
            return ProvenEnumClass::None;
        };
        let class = match class {
            Ok(class) => class,
            Err(reason) => return ProvenEnumClass::Unsupported(reason),
        };
        let Some(identity) =
            declaration_signature(&class.symbol).map(|declaration| KlibDependencyCallable {
                module,
                declaration,
            })
        else {
            return ProvenEnumClass::Unsupported("an enum class without a public signature");
        };
        ProvenEnumClass::Simple {
            identity,
            entries: class.entries,
        }
    }

    /// The one simple enum `classifier` among the loaded libraries, each searched only in its own
    /// trees. A producer graph is not used: a dependent library must not count the same class a
    /// second time.
    pub(crate) fn proven_loaded_enum(&self, classifier: TypeName) -> ProvenEnumClass {
        let mut found = None;
        for (module, &library) in &self.modules {
            for tree in self.libraries[library].trees() {
                let Some(class) = find_simple_enum(&tree.arena, &tree.declaration, classifier)
                else {
                    continue;
                };
                if found.is_some() {
                    return ProvenEnumClass::Several;
                }
                found = Some((module.clone(), class));
            }
        }
        let Some((module, class)) = found else {
            return ProvenEnumClass::None;
        };
        let class = match class {
            Ok(class) => class,
            Err(reason) => return ProvenEnumClass::Unsupported(reason),
        };
        let Some(identity) =
            declaration_signature(&class.symbol).map(|declaration| KlibDependencyCallable {
                module,
                declaration,
            })
        else {
            return ProvenEnumClass::Unsupported("an enum class without a public signature");
        };
        ProvenEnumClass::Simple {
            identity,
            entries: class.entries,
        }
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

    /// Exact references consumed by prepared template linking. No inherited alias enters the
    /// declaration index or the narrower intrinsic-support call graph.
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

    /// Resolve a call symbol in the graph of the KLIB body which serialized it.
    ///
    /// A definition in the body module wins. Otherwise exactly one reachable producer module
    /// must define the signature. Input order and equal declarations outside that graph have no
    /// bearing on the answer.
    pub(crate) fn callee_origin(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> KlibCalleeOrigin {
        let signature = ir_signature(declaration);
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
            {
                if owner.replace(module.clone()).is_some() {
                    return KlibCalleeOrigin::Ambiguous;
                }
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
        let signature = ir_signature(&identity.declaration);
        let library = *self
            .owners
            .get(&(identity.module.clone(), signature.clone()))?;
        self.libraries[library].function(&signature)
    }

    /// Whether the exact serialized declaration intentionally carries an executable body.
    pub(crate) fn function_has_body(&self, identity: &KlibDependencyCallable) -> Option<bool> {
        self.function(identity)
            .map(|(_, function)| function.body.is_some())
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
                |declaration| match self.callee_origin(&identity.module, &declaration) {
                    KlibCalleeOrigin::Exact(callee) => Some(callee),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                },
            )
            .collect()
    }

    /// Exact module-qualified edges of the bodies common lowering structurally emits, including
    /// reached closure implementations. The supplied operation policy does not grant intrinsic
    /// authority; original direct inventories remain the authority for that separate decision.
    pub(crate) fn emitted_body_callables(
        &self,
        identity: &KlibDependencyCallable,
        forms: crate::metadata::klib_ir::tree::EmittedCallForms,
    ) -> Vec<KlibDependencyCallable> {
        let Some((arena, function)) = self.function(identity) else {
            return Vec::new();
        };
        arena
            .emitted_callable_symbols(function, forms)
            .into_iter()
            .filter_map(|symbol| declaration_signature(&symbol.signature))
            .filter_map(
                |declaration| match self.callee_origin(&identity.module, &declaration) {
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
                self.callee_origin(&identity.module, &declaration)
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

struct SimpleEnumClass {
    symbol: KlibIrSignature,
    entries: Vec<String>,
}

fn find_simple_enum(
    arena: &KlibIrArena,
    member: &KlibIrMember,
    classifier: TypeName,
) -> Option<Result<SimpleEnumClass, &'static str>> {
    let mut found = None;
    walk_simple_enum(arena, member, classifier, &mut found);
    found
}

fn walk_simple_enum(
    arena: &KlibIrArena,
    member: &KlibIrMember,
    classifier: TypeName,
    found: &mut Option<Result<SimpleEnumClass, &'static str>>,
) {
    let KlibIrMember::Class(id) = member else {
        return;
    };
    let class = arena.class(*id);
    if class.base.symbol.kind == KlibIrSymbolKind::Class
        && class_identity(&class.base.symbol.signature) == Some(classifier)
    {
        let proven = prove_simple_enum(arena, class);
        *found = Some(match found.take() {
            Some(_) => Err("an enum class is serialized more than once"),
            None => proven,
        });
    }
    for member in &class.members {
        walk_simple_enum(arena, member, classifier, found);
        if let KlibIrMember::EnumEntry(entry) = member {
            if let Some(nested) = entry.class {
                walk_simple_enum(arena, &KlibIrMember::Class(nested), classifier, found);
            }
        }
    }
}

/// `class` is the serialized enum `kotlin.Enum` when every constant is argument-less and body-less.
fn prove_simple_enum(
    arena: &KlibIrArena,
    class: &crate::metadata::klib_ir::tree::KlibIrClass,
) -> Result<SimpleEnumClass, &'static str> {
    if !class.type_parameters.is_empty() || class.value_class_representation.is_some() {
        return Err("an enum class with type parameters or a value representation");
    }
    let mut supertypes = class.supertypes.iter().copied();
    let Some(supertype) = supertypes.next().filter(|_| supertypes.next().is_none()) else {
        return Err("an enum class without exactly one supertype");
    };
    if !is_kotlin_enum(arena, supertype, class) {
        return Err("a class that does not extend kotlin.Enum");
    }
    let mut constructors = class.members.iter().filter_map(|member| match member {
        KlibIrMember::Function(function) if arena.function(*function).constructor => {
            Some(arena.function(*function))
        }
        _ => None,
    });
    let Some(constructor) = constructors.next() else {
        return Err("an enum class without its constructor");
    };
    if constructors.next().is_some() || !simple_enum_class_constructor(arena, constructor, class) {
        return Err("an enum class with a nontrivial constructor");
    }
    let mut entries = Vec::new();
    for member in &class.members {
        match member {
            KlibIrMember::EnumEntry(entry) => {
                if entry.class.is_some() {
                    return Err("an enum entry with a body");
                }
                let Some(initializer) = entry.initializer else {
                    return Err("an enum entry without its constructor initializer");
                };
                simple_enum_constructor(arena, initializer, class, constructor)?;
                if entry.name.is_empty() || entries.iter().any(|name| name == &entry.name) {
                    return Err("an enum entry without a unique name");
                }
                entries.push(entry.name.clone());
            }
            KlibIrMember::Function(function) => {
                let function = arena.function(*function);
                if !function.constructor && !simple_enum_member_body(function) {
                    return Err("an enum class with an executable member body");
                }
            }
            KlibIrMember::Field(_) => return Err("an enum class with additional field storage"),
            KlibIrMember::Property(property) => {
                if property.backing_field.is_some() {
                    return Err("an enum class with additional property storage");
                }
                if [property.getter, property.setter]
                    .into_iter()
                    .flatten()
                    .any(|accessor| !simple_enum_member_body(arena.function(accessor)))
                {
                    return Err("an enum class with an executable accessor body");
                }
            }
            KlibIrMember::AnonymousInitializer { statements, .. } if !statements.is_empty() => {
                return Err("an enum class with an initializer");
            }
            KlibIrMember::Class(_) => {
                return Err("an enum class with a nested classifier");
            }
            _ => {}
        }
    }
    if entries.is_empty() {
        return Err("an enum class with no entries");
    }
    Ok(SimpleEnumClass {
        symbol: class.base.symbol.signature.clone(),
        entries,
    })
}

/// `kotlin.Enum<ThisClass>`: the one supertype a Kotlin enum class records, with the class itself
/// as the invariant type argument.
fn is_kotlin_enum(
    arena: &KlibIrArena,
    ty: crate::metadata::klib_ir::tree::KlibIrTypeId,
    class: &crate::metadata::klib_ir::tree::KlibIrClass,
) -> bool {
    let KlibIrType::Simple {
        classifier,
        nullability,
        arguments,
        abbreviation: None,
        ..
    } = arena.ty(ty)
    else {
        return false;
    };
    if !matches!(
        nullability,
        crate::metadata::klib_ir::tree::KlibIrNullability::DefinitelyNotNull
            | crate::metadata::klib_ir::tree::KlibIrNullability::NotSpecified
    ) || classifier.kind != KlibIrSymbolKind::Class
        || class_identity(&classifier.signature) != Some(crate::types::wk::kotlin_enum())
    {
        return false;
    }
    let [crate::metadata::klib_ir::tree::KlibIrTypeArgument::Type {
        variance: crate::metadata::klib_ir::tree::KlibIrVariance::Invariant,
        ty: argument,
    }] = arguments.as_slice()
    else {
        return false;
    };
    this_class(arena, *argument, class)
}

fn this_class(
    arena: &KlibIrArena,
    ty: crate::metadata::klib_ir::tree::KlibIrTypeId,
    class: &crate::metadata::klib_ir::tree::KlibIrClass,
) -> bool {
    matches!(
        arena.ty(ty),
        KlibIrType::Simple {
            classifier,
            nullability: crate::metadata::klib_ir::tree::KlibIrNullability::DefinitelyNotNull
                | crate::metadata::klib_ir::tree::KlibIrNullability::NotSpecified,
            arguments,
            abbreviation: None,
            ..
        } if arguments.is_empty()
            && classifier.kind == KlibIrSymbolKind::Class
            && class_identity(&classifier.signature) == class_identity(&class.base.symbol.signature)
    )
}

fn simple_enum_constructor(
    arena: &KlibIrArena,
    initializer: crate::metadata::klib_ir::tree::KlibIrExprId,
    class: &crate::metadata::klib_ir::tree::KlibIrClass,
    constructor: &KlibIrFunction,
) -> Result<(), &'static str> {
    let KlibIrExprKind::EnumConstructorCall(access) = &arena.expr(initializer).kind else {
        return Err("an enum entry whose initializer is not its constructor");
    };
    if access.type_arguments.iter().any(Option::is_some) || !arguments_are_absent(&access.arguments)
    {
        return Err("an enum entry with a constructor argument");
    }
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || access.symbol != constructor.base.symbol
        || declaration_owner(&access.symbol.signature)
            != class_identity(&class.base.symbol.signature)
    {
        return Err("an enum entry constructed by another class or constructor");
    }
    Ok(())
}

/// The shared realization supplies only Enum's name/ordinal initialization. A serialized
/// constructor may perform that delegation and initialize its own otherwise inert instance,
/// but no user code or defaulted operands may be discarded.
fn simple_enum_class_constructor(
    arena: &KlibIrArena,
    constructor: &KlibIrFunction,
    class: &crate::metadata::klib_ir::tree::KlibIrClass,
) -> bool {
    if constructor.base.symbol.kind != KlibIrSymbolKind::Constructor
        || declaration_owner(&constructor.base.symbol.signature)
            != class_identity(&class.base.symbol.signature)
        || !constructor.type_parameters.is_empty()
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
    let KlibIrExprKind::EnumConstructorCall(access) = &arena.expr(*delegate).kind else {
        return false;
    };
    let [Some(argument)] = access.type_arguments.as_slice() else {
        return false;
    };
    access.symbol.kind == KlibIrSymbolKind::Constructor
        && declaration_owner(&access.symbol.signature) == Some(crate::types::wk::kotlin_enum())
        && arguments_are_absent(&access.arguments)
        && this_class(arena, *argument, class)
        && matches!(&arena.expr(*initialize).kind,
            KlibIrExprKind::InstanceInitializerCall(initialized) if initialized == &class.base.symbol)
}

/// Inherited declarations have no serialized body; the three enum synthetic roles are supplied
/// by the target's ordinary enum realization. An executable override/accessor needs its own body
/// and override graph, which this restricted realization does not install.
fn simple_enum_member_body(function: &KlibIrFunction) -> bool {
    match function.body {
        Some(KlibIrBody::Synthetic(_)) => true,
        Some(KlibIrBody::Block(_)) => false,
        None => {
            // Body absence also describes external declarations. Only an explicitly serialized
            // inherited role with exact Enum/Any declaration edges is supplied by this layout.
            function.base.origin == "FAKE_OVERRIDE"
                && !function.overridden.is_empty()
                && function.overridden.iter().all(|symbol| {
                    let signature = match &symbol.signature {
                        KlibIrSignature::Public(signature) => signature,
                        KlibIrSignature::Accessor(signature) => signature.property(),
                        KlibIrSignature::FileLocal { .. } => return false,
                    };
                    symbol.kind == KlibIrSymbolKind::Function
                        && matches!(
                            declaration_owner(&KlibIrSignature::Public(signature.clone())),
                            Some(owner) if owner == crate::types::wk::kotlin_enum()
                                || owner == crate::types::wk::any()
                        )
                })
        }
    }
}

fn find_enum_entry(
    arena: &KlibIrArena,
    member: &KlibIrMember,
    symbol: &KlibIrSymbol,
    enclosing: Option<TypeName>,
    found: &mut Option<(TypeName, String)>,
    several: &mut bool,
) {
    match member {
        KlibIrMember::Class(id) => {
            let class = arena.class(*id);
            let identity = class_identity(&class.base.symbol.signature);
            for member in &class.members {
                find_enum_entry(arena, member, symbol, identity, found, several);
            }
        }
        KlibIrMember::EnumEntry(entry) => {
            if &entry.base.symbol == symbol {
                match enclosing {
                    Some(classifier) => {
                        if found.replace((classifier, entry.name.clone())).is_some() {
                            *several = true;
                        }
                    }
                    None => *several = true,
                }
            }
            if let Some(class) = entry.class {
                find_enum_entry(
                    arena,
                    &KlibIrMember::Class(class),
                    symbol,
                    enclosing,
                    found,
                    several,
                );
            }
        }
        _ => {}
    }
}

fn find_class(
    arena: &KlibIrArena,
    member: &KlibIrMember,
    classifier: TypeName,
    found: &mut Option<bool>,
) {
    let KlibIrMember::Class(id) = member else {
        return;
    };
    let class = arena.class(*id);
    if class.base.symbol.kind == KlibIrSymbolKind::Class
        && class_identity(&class.base.symbol.signature) == Some(classifier)
    {
        let mut constructors = class.members.iter().filter_map(|member| match member {
            KlibIrMember::Function(function) if arena.function(*function).constructor => {
                Some(arena.function(*function))
            }
            _ => None,
        });
        let inert = class.supertypes.len() == 1
            && inert_any_supertype(arena, class.supertypes[0])
            && constructors.next().is_some_and(|constructor| {
                inert_constructor(arena, constructor, &class.base.symbol)
            })
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
            });
        *found = Some(found.unwrap_or(true) && inert);
    }
    for member in &class.members {
        find_class(arena, member, classifier, found);
    }
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

fn ir_signature(declaration: &KlibDeclarationSignature) -> KlibIrSignature {
    match declaration {
        KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature.clone()),
        KlibDeclarationSignature::Accessor(signature) => {
            KlibIrSignature::Accessor(signature.clone())
        }
    }
}

fn declaration_signature(signature: &KlibIrSignature) -> Option<KlibDeclarationSignature> {
    match signature {
        KlibIrSignature::Public(signature) => {
            Some(KlibDeclarationSignature::Public(signature.clone()))
        }
        KlibIrSignature::Accessor(signature) => {
            Some(KlibDeclarationSignature::Accessor(signature.clone()))
        }
        KlibIrSignature::FileLocal { .. } => None,
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

    fn simple_enum_fixture() -> (KlibIrArena, crate::metadata::klib_ir::tree::KlibIrClass) {
        use crate::metadata::klib_ir::tree::{
            KlibIrClass, KlibIrEnumEntry, KlibIrTypeArgument, KlibIrVariance,
        };
        let mut arena = KlibIrArena::default();
        let symbol = class_symbol("fixture", "Enum");
        let own_type = arena.push_type(KlibIrType::Simple {
            classifier: symbol.clone(),
            nullability: KlibIrNullability::DefinitelyNotNull,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        let super_type = arena.push_type(KlibIrType::Simple {
            classifier: class_symbol("kotlin", "Enum"),
            nullability: KlibIrNullability::DefinitelyNotNull,
            arguments: vec![KlibIrTypeArgument::Type {
                variance: KlibIrVariance::Invariant,
                ty: own_type,
            }],
            annotations: Vec::new(),
            abbreviation: None,
        });
        let mut constructor = empty_constructor(
            &mut arena,
            symbol.clone(),
            constructor_symbol("kotlin", "Enum"),
        );
        constructor.base.symbol = constructor_symbol("fixture", "Enum");
        constructor.return_type = own_type;
        let delegate = arena.push_expr(
            None,
            KlibIrExprKind::EnumConstructorCall(KlibIrMemberAccess {
                symbol: constructor_symbol("kotlin", "Enum"),
                arguments: KlibIrArguments::Flat(Vec::new()),
                type_arguments: vec![Some(own_type)],
                origin: None,
            }),
        );
        let initialize = arena.push_expr(
            None,
            KlibIrExprKind::InstanceInitializerCall(symbol.clone()),
        );
        constructor.body = Some(KlibIrBody::Block(vec![
            KlibIrStatement::Expression(delegate),
            KlibIrStatement::Expression(initialize),
        ]));
        let constructor_symbol = constructor.base.symbol.clone();
        let constructor = arena.push_function(constructor);
        let base = KlibIrDeclarationBase {
            symbol: symbol.clone(),
            origin: "DEFINED".to_owned(),
            flags: 0,
            annotations: Vec::new(),
        };
        let mut members = vec![KlibIrMember::Function(constructor)];
        for name in ["FIRST", "SECOND"] {
            let initializer = arena.push_expr(
                None,
                KlibIrExprKind::EnumConstructorCall(KlibIrMemberAccess {
                    symbol: constructor_symbol.clone(),
                    arguments: KlibIrArguments::Flat(Vec::new()),
                    type_arguments: Vec::new(),
                    origin: None,
                }),
            );
            members.push(KlibIrMember::EnumEntry(KlibIrEnumEntry {
                base: base.clone(),
                name: name.to_owned(),
                initializer: Some(initializer),
                class: None,
            }));
        }
        (
            arena,
            KlibIrClass {
                base,
                name: "Enum".to_owned(),
                this_receiver: None,
                type_parameters: Vec::new(),
                supertypes: vec![super_type],
                members,
                value_class_representation: None,
                sealed_subclasses: Vec::new(),
            },
        )
    }

    #[test]
    fn simple_enum_keeps_implicit_construction_order_and_synthetic_roles() {
        use crate::metadata::klib_ir::tree::KlibIrSyntheticBody;
        let (mut arena, mut class) = simple_enum_fixture();
        let KlibIrMember::Function(constructor) = class.members[0] else {
            unreachable!()
        };
        for role in [
            KlibIrSyntheticBody::EnumValues,
            KlibIrSyntheticBody::EnumValueOf,
            KlibIrSyntheticBody::EnumEntries,
        ] {
            let mut generated = arena.function(constructor).clone();
            generated.constructor = false;
            generated.base.symbol.kind = KlibIrSymbolKind::Function;
            generated.body = Some(KlibIrBody::Synthetic(role));
            class
                .members
                .push(KlibIrMember::Function(arena.push_function(generated)));
        }
        let mut inherited = arena.function(constructor).clone();
        inherited.constructor = false;
        inherited.base.symbol.kind = KlibIrSymbolKind::Function;
        inherited.base.origin = "FAKE_OVERRIDE".to_owned();
        let package = ["kotlin".to_owned()];
        let owners = [ClassScope::<KotlinType> {
            name: "Enum",
            type_parameters: Vec::new(),
            expect: false,
        }];
        let inherited_signature = callable_signature(
            DeclarationContainer {
                package: &package,
                classes: &owners,
                native_interop_library: false,
            },
            &CallableShape {
                name: "hashCode",
                contexts: Vec::new(),
                receiver: None,
                params: Vec::new(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("Enum's no-argument hashCode is signable");
        inherited.name = "hashCode".to_owned();
        inherited.overridden.push(KlibIrSymbol {
            kind: KlibIrSymbolKind::Function,
            signature: KlibIrSignature::Public(inherited_signature),
        });
        inherited.body = None;
        class
            .members
            .push(KlibIrMember::Function(arena.push_function(inherited)));
        assert_eq!(
            prove_simple_enum(&arena, &class)
                .expect("bare enum")
                .entries,
            ["FIRST".to_owned(), "SECOND".to_owned()]
        );
    }

    #[test]
    fn simple_enum_declines_discarded_field_and_property_initialization() {
        use crate::metadata::klib_ir::tree::{KlibIrField, KlibIrProperty};
        let (mut arena, mut class) = simple_enum_fixture();
        let effect = arena.push_expr(None, KlibIrExprKind::GetObject(class.base.symbol.clone()));
        let field = KlibIrField {
            base: class.base.clone(),
            name: "payload".to_owned(),
            ty: class.supertypes[0],
            initializer: Some(effect),
        };
        class.members.push(KlibIrMember::Property(KlibIrProperty {
            base: class.base.clone(),
            name: "payload".to_owned(),
            backing_field: Some(field.clone()),
            getter: None,
            setter: None,
        }));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with additional property storage")
        );
        class.members.pop();
        class.members.push(KlibIrMember::Field(field));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with additional field storage")
        );
    }

    #[test]
    fn simple_enum_declines_constructor_effects_and_omitted_default_operands() {
        use crate::metadata::klib_ir::tree::KlibIrParameter;
        let (mut arena, mut class) = simple_enum_fixture();
        let KlibIrMember::Function(id) = class.members[0] else {
            unreachable!()
        };
        let original = arena.function(id).clone();
        let effect = arena.push_expr(None, KlibIrExprKind::GetObject(class.base.symbol.clone()));
        let mut constructor = original.clone();
        let Some(KlibIrBody::Block(statements)) = constructor.body.as_mut() else {
            unreachable!()
        };
        statements.push(KlibIrStatement::Expression(effect));
        class.members[0] = KlibIrMember::Function(arena.push_function(constructor));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with a nontrivial constructor")
        );
        let mut constructor = original;
        constructor.regular_parameters.push(KlibIrParameter {
            base: class.base.clone(),
            name: "value".to_owned(),
            ty: constructor.return_type,
            vararg_element_type: None,
            default_value: Some(effect),
        });
        class.members[0] = KlibIrMember::Function(arena.push_function(constructor));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with a nontrivial constructor")
        );
    }

    #[test]
    fn simple_enum_declines_executable_overrides_and_accessors() {
        use crate::metadata::klib_ir::tree::KlibIrProperty;
        let (mut arena, mut class) = simple_enum_fixture();
        let KlibIrMember::Function(id) = class.members[0] else {
            unreachable!()
        };
        let mut executable = arena.function(id).clone();
        executable.constructor = false;
        executable.base.symbol.kind = KlibIrSymbolKind::Function;
        let executable = arena.push_function(executable);
        class.members.push(KlibIrMember::Function(executable));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with an executable member body")
        );
        class.members.pop();
        class.members.push(KlibIrMember::Property(KlibIrProperty {
            base: class.base.clone(),
            name: "computed".to_owned(),
            backing_field: None,
            getter: Some(executable),
            setter: None,
        }));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with an executable accessor body")
        );
    }
    #[test]
    fn simple_enum_requires_its_own_instance_and_each_actual_entry_initializer() {
        let (mut arena, mut class) = simple_enum_fixture();
        let KlibIrMember::Function(id) = class.members[0] else {
            unreachable!()
        };
        let original = arena.function(id).clone();
        let mut constructor = original;
        let foreign = arena.push_expr(
            None,
            KlibIrExprKind::InstanceInitializerCall(class_symbol("fixture", "Other")),
        );
        let Some(KlibIrBody::Block(statements)) = constructor.body.as_mut() else {
            unreachable!()
        };
        statements[1] = KlibIrStatement::Expression(foreign);
        class.members[0] = KlibIrMember::Function(arena.push_function(constructor));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with a nontrivial constructor")
        );
        class.members[0] = KlibIrMember::Function(id);
        let KlibIrMember::EnumEntry(entry) = &mut class.members[1] else {
            unreachable!()
        };
        entry.initializer = None;
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum entry without its constructor initializer")
        );
    }
    #[test]
    fn simple_enum_declines_bodyless_external_members_and_unproven_inheritance() {
        let (mut arena, mut class) = simple_enum_fixture();
        let KlibIrMember::Function(id) = class.members[0] else {
            unreachable!()
        };
        let mut external = arena.function(id).clone();
        external.constructor = false;
        external.base.symbol.kind = KlibIrSymbolKind::Function;
        external.body = None;
        class.members.push(KlibIrMember::Function(
            arena.push_function(external.clone()),
        ));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with an executable member body")
        );
        class.members.pop();
        external.base.origin = "FAKE_OVERRIDE".to_owned();
        class
            .members
            .push(KlibIrMember::Function(arena.push_function(external)));
        assert_eq!(
            prove_simple_enum(&arena, &class).err(),
            Some("an enum class with an executable member body")
        );
    }
}
