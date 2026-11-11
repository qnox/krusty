//! Library objects this module materializes itself instead of running their serialized bodies:
//! the continuation `Continuation(context) { ... }` builds, and the canonical instance of a
//! dependency object whose published declaration stores nothing and whose module exported no
//! instance. Each is created once per module, so two reads of the same object see one instance.
//!
//! The runtime knows nothing about Kotlin declarations, so the per-classifier table lives here
//! beside it; [`ModuleRuntime`] dereferences to the runtime for everything else.

use std::ops::{Deref, DerefMut};

use krusty::fx_hash::HashMap;
use krusty::types::TypeName;

use super::super::runtime::Runtime;

/// The runtime together with the library objects lowered against it.
pub(super) struct ModuleRuntime {
    runtime: Runtime,
    pub(super) library_objects: LibraryObjects,
}

impl ModuleRuntime {
    pub(super) fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            library_objects: LibraryObjects::default(),
        }
    }
}

impl Deref for ModuleRuntime {
    type Target = Runtime;

    fn deref(&self) -> &Runtime {
        &self.runtime
    }
}

impl DerefMut for ModuleRuntime {
    fn deref_mut(&mut self) -> &mut Runtime {
        &mut self.runtime
    }
}

#[derive(Default)]
pub(super) struct LibraryObjects {
    /// The continuation object: its struct, and the global holding its dispatch table.
    pub(super) continuation: Option<(u32, u32)>,
    /// Struct and dispatch table shared by canonical dependency objects that store nothing.
    pub(super) inert_layout: Option<(u32, u32)>,
    /// One global instance per such object.
    pub(super) inert_objects: HashMap<TypeName, u32>,
}
