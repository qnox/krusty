//! The library-set abstraction — one half of a target *platform* (the other half is its emitter,
//! e.g. `jvm::JvmBackend`). A `LibrarySet` is the common denominator a front end needs from a
//! target's compiled libraries: the type universe plus member resolution, whether those libraries
//! are a JVM classpath (bytecode `.class` jars) or a klib (serialized IR).
//!
//! The resolver and IR lowering depend **only** on this trait, never on the JVM backend: every
//! `java/lang/…` name, descriptor parse, and classpath read lives behind a concrete implementation
//! (`jvm::jvm_libraries::JvmLibraries`). Swapping in a klib-backed `LibrarySet` would let the same
//! front end target Kotlin/JS without touching `resolve`/`ir_lower`.

use crate::types::Ty;
use std::collections::HashMap;

/// The type universe the library set contributes, resolved to internal names: every importable
/// simple name → its internal name, plus type aliases (`alias` → target simple/internal name).
#[derive(Default)]
pub struct LibrarySeed {
    pub class_names: HashMap<String, String>,
    pub type_aliases: HashMap<String, String>,
}

/// What the front end asks of the target's library set. Results are in Kotlin terms (`Ty`, internal
/// names); any backend-specific encoding (a JVM method descriptor) is an opaque string the matching
/// backend emitter consumes. Default methods resolve nothing, for an empty library set.
pub trait LibrarySet {
    /// The seed type universe (classpath/klib types + intrinsic built-in mappings).
    fn seed(&self) -> LibrarySeed {
        LibrarySeed::default()
    }

    /// Members `(name, Ty)` of an annotation type, if `internal` names one in the library set.
    fn annotation_members(&self, _internal: &str) -> Option<Vec<(String, Ty)>> {
        None
    }

    /// Resolve a static call `Owner.method(args)`. Returns `(owner internal, method descriptor,
    /// return Ty)`.
    fn resolve_static(&self, _internal: &str, _method: &str, _arg_tys: &[Ty]) -> Option<(String, String, Ty)> {
        None
    }

    /// Resolve an instance method on a library type. Returns `(method descriptor, return Ty)`.
    fn resolve_instance(&self, _internal: &str, _method: &str, _arg_tys: &[Ty]) -> Option<(String, Ty)> {
        None
    }

    /// Resolve a constructor on a library type by argument types. Returns its descriptor.
    fn resolve_ctor(&self, _internal: &str, _arg_tys: &[Ty]) -> Option<String> {
        None
    }

    /// Resolve an extension / static method taking `receiver` as its first argument. Returns
    /// `(owner internal, method name, descriptor, return Ty)`.
    fn resolve_extension(&self, _receiver: Ty, _method: &str, _arg_tys: &[Ty]) -> Option<(String, String, String, Ty)> {
        None
    }

    /// Parse a library member descriptor to a `Ty` (a JVM field/return descriptor for the JVM
    /// library set). Only reached for types the library set itself surfaced.
    fn desc_to_ty(&self, _desc: &str) -> Ty {
        Ty::Error
    }

    /// Whether `internal` names an interface in the library set (governs virtual vs. interface dispatch).
    fn is_interface(&self, _internal: &str) -> bool {
        false
    }
}

/// A library set with no external libraries — compiling a self-contained source set with no classpath.
pub struct EmptyLibrarySet;

impl LibrarySet for EmptyLibrarySet {}
