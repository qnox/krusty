//! The serialized IR bodies of the declarations a set of KLIB libraries publishes.
//!
//! A selected dependency callable reaches its body through the exact serialized identity its
//! provider published with it (`KlibDeclarationSignature`), never through a name, an owner, or a
//! parameter tuple. Each identity belongs to exactly one library: two libraries that both define
//! one signature leave no single body to join, so the set is rejected when it is built.

use std::collections::HashMap;

use crate::libraries::KlibDeclarationSignature;
use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrFunction};
use crate::metadata::klib_ir::{KlibIrModuleTrees, KlibIrSignature};

/// The decoded function declarations of a set of KLIB libraries, joined by exact signature.
pub struct KlibDeclarationBodies {
    libraries: Vec<KlibIrModuleTrees>,
    /// Which library defines each linkable identity.
    owners: HashMap<KlibIrSignature, usize>,
}

/// Two libraries of one set define the same linkable identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibDeclarationBodiesError {
    signature: KlibIrSignature,
    libraries: [usize; 2],
}

impl KlibDeclarationBodiesError {
    pub fn signature(&self) -> &KlibIrSignature {
        &self.signature
    }

    /// The positions, in the input order, of the two libraries that define the identity.
    pub fn libraries(&self) -> [usize; 2] {
        self.libraries
    }
}

impl std::fmt::Display for KlibDeclarationBodiesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [first, second] = self.libraries;
        write!(
            formatter,
            "KLIB libraries {first} and {second} both define the declaration {:?}",
            self.signature
        )
    }
}

impl std::error::Error for KlibDeclarationBodiesError {}

impl KlibDeclarationBodies {
    /// Index the decoded trees of each library. A signature two libraries define rejects the set.
    pub fn from_libraries(
        libraries: Vec<KlibIrModuleTrees>,
    ) -> Result<Self, KlibDeclarationBodiesError> {
        let mut owners = HashMap::new();
        for (library, trees) in libraries.iter().enumerate() {
            for signature in trees.function_signatures() {
                if let Some(previous) = owners.insert(signature.clone(), library) {
                    return Err(KlibDeclarationBodiesError {
                        signature: signature.clone(),
                        libraries: [previous, library],
                    });
                }
            }
        }
        Ok(Self { libraries, owners })
    }

    /// The function declared under exactly `signature`, with the arena that owns its body.
    pub fn function(
        &self,
        signature: &KlibDeclarationSignature,
    ) -> Option<(&KlibIrArena, &KlibIrFunction)> {
        let signature = match signature {
            KlibDeclarationSignature::Public(signature) => {
                KlibIrSignature::Public(signature.clone())
            }
            KlibDeclarationSignature::Accessor(signature) => {
                KlibIrSignature::Accessor(signature.clone())
            }
        };
        let library = *self.owners.get(&signature)?;
        self.libraries[library].function(&signature)
    }
}
