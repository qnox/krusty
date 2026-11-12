//! The module-owned Kotlin exception tag and base Throwable storage.
//!
//! A throw transports the very same GC reference; no host exception wrapper or replacement Kotlin
//! object crosses a catch boundary. The base carrier extends `$Identified`, the prefix of every
//! object with Kotlin's identity semantics (`super::identity`), so an exception has an identity
//! hash like any other object; its message and cause follow.
//!
//! Each runtime exception class has a dispatch table of its own, whose `toString` renders that
//! class's name (`kotlin.Throwable.toString`). The registry declares a class's struct; the runtime
//! then gives the class its table ([`ExceptionTransport::declared_classes`],
//! [`ExceptionTransport::publish_table`]) before anything constructs it.

use super::encode::{
    Code, CompositeType, FieldType, HeapType, Module, StorageType, SubType, ValType,
};
use super::identity::{IdentityObjects, PREFIX_FIELDS};
use super::objects::{immutable, ObjectRoot, REFERENCE};
use krusty::fx_hash::HashMap;

mod class_registry;

pub(super) struct ExceptionTransport {
    pub(super) tag: u32,
    structure: u32,
    /// Gives a new object its identity hash.
    allocate_hash: u32,
    classes: class_registry::RuntimeClasses,
    /// The dispatch table global of each declared class, by its struct.
    tables: HashMap<u32, u32>,
}

impl ExceptionTransport {
    pub(super) fn declare(
        module: &mut Module,
        objects: &ObjectRoot,
        identity: &IdentityObjects,
        string: ValType,
    ) -> Self {
        let tag = module.exception_tag(vec![REFERENCE]);
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(Self::fields(objects.any_table, string)),
            supertype: Some(objects.identified),
            open: true,
        });
        Self {
            tag,
            structure,
            allocate_hash: identity.allocate,
            classes: class_registry::RuntimeClasses::new(structure, objects.any_table, string),
            tables: HashMap::default(),
        }
    }

    /// The fields of every runtime exception struct: `$Identified`'s, then the message and cause.
    fn fields(table: u32, string: ValType) -> Vec<FieldType> {
        [
            immutable(ValType::reference(table)),
            immutable(ValType::I32),
        ]
        .into_iter()
        .chain(Self::base_fields(string))
        .collect()
    }

    /// The message and cause every exception struct carries after its identity prefix. A
    /// constructor of a class extending a runtime exception writes them once its object exists,
    /// so they are mutable.
    pub(super) fn base_fields(string: ValType) -> [FieldType; 2] {
        [string, REFERENCE].map(|value| FieldType {
            storage: StorageType::Val(value),
            mutable: true,
        })
    }

    /// Store the `message` and `cause` locals into the exception fields of the `object` local.
    pub(super) fn store_base(&self, code: &mut Code, object: u32, message: u32, cause: u32) {
        code.local_get(object)
            .ref_cast(false, HeapType::Concrete(self.structure))
            .local_get(message)
            .struct_set(self.structure, MESSAGE_FIELD)
            .local_get(object)
            .ref_cast(false, HeapType::Concrete(self.structure))
            .local_get(cause)
            .struct_set(self.structure, CAUSE_FIELD);
    }

    pub(super) fn ensure_class(
        &mut self,
        module: &mut Module,
        source: &dyn krusty::backend::BackendClassifierSource,
        identity: krusty::types::TypeName,
    ) -> Result<u32, String> {
        self.classes.ensure(module, source, identity)
    }

    /// The classes [`Self::ensure_class`] declared since the last call, each with its struct, to
    /// be given their tables.
    pub(super) fn declared_classes(&mut self) -> Vec<(krusty::types::TypeName, u32)> {
        self.classes.take_declared()
    }

    /// Record `table` as the dispatch table of the class whose struct is `structure`.
    pub(super) fn publish_table(&mut self, structure: u32, table: u32) {
        self.tables.insert(structure, table);
    }

    /// The nullable message is already evaluated into a local. These constructors have no cause;
    /// preserve the shared cause slot and allocate the authenticated selected subtype.
    pub(super) fn construct_class(
        &self,
        code: &mut Code,
        structure: u32,
        message: Option<u32>,
        string: u32,
    ) -> Result<(), String> {
        let table = self
            .tables
            .get(&structure)
            .ok_or("a runtime exception class constructed before its table")?;
        code.global_get(*table).call(self.allocate_hash);
        match message {
            Some(message) => code.local_get(message),
            None => code.ref_null(HeapType::Concrete(string)),
        };
        code.ref_null(HeapType::Eq).struct_new(structure);
        Ok(())
    }

    pub(super) fn message(&self, code: &mut Code) {
        code.ref_cast(false, HeapType::Concrete(self.structure))
            .struct_get(self.structure, MESSAGE_FIELD);
    }

    /// Replace the throwable on the stack with its `cause`.
    pub(super) fn cause(&self, code: &mut Code) {
        code.ref_cast(false, HeapType::Concrete(self.structure))
            .struct_get(self.structure, CAUSE_FIELD);
    }
}

/// The exception struct fields holding the message and the cause.
const MESSAGE_FIELD: u32 = PREFIX_FIELDS;
const CAUSE_FIELD: u32 = PREFIX_FIELDS + 1;
