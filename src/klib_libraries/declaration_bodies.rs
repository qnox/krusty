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
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction};
use crate::metadata::klib_ir::{KlibIrModuleTrees, KlibIrSignature};

/// The decoded function declarations of a set of KLIB libraries, joined by exact signature.
pub struct KlibDeclarationBodies {
    libraries: Vec<KlibIrModuleTrees>,
    /// Which library defines each linkable identity.
    owners: HashMap<(String, KlibIrSignature), usize>,
    /// The producer-declared module graph. These are manifest `depends` edges, not the roles the
    /// current compilation assigned its inputs.
    dependencies: HashMap<String, Vec<String>>,
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
            owners,
            dependencies: dependencies
                .into_iter()
                .map(|(module, dependencies)| (module, dependencies.into_iter().collect()))
                .collect(),
        })
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
}

fn ir_signature(declaration: &KlibDeclarationSignature) -> KlibIrSignature {
    match declaration {
        KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature.clone()),
        KlibDeclarationSignature::Accessor(signature) => {
            KlibIrSignature::Accessor(signature.clone())
        }
    }
}
