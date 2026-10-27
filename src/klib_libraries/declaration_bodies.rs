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
use crate::metadata::klib_ir::{KlibIrModuleTrees, KlibIrSignature, KlibIrSymbolKind};
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
    /// literal field initialization and an empty anonymous initializer. An absent, multiply owned,
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
                KlibIrMember::Function(function) => arena.function(*function).constructor,
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
}
