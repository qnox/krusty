//! Common-IR record of `companion { … }` block members.
//!
//! A block member is a static member of the classifier that declares the block. Common lowering
//! places block functions and property accessors among that class's methods and records here which
//! functions, statics and properties came from a block, so the backend realizes them as class
//! statics and records them in the class's `@Metadata`. A written `companion fun C.f()` /
//! `companion val C.p` is a package declaration and is not recorded here.

use std::collections::{HashMap, HashSet};

use super::{ClassId, FunId, IrModuleProperty, IrPackageProperty, Ty, TypeName};

#[derive(Clone, Debug, Default)]
pub struct IrCompanionBlocks {
    /// Function → the class whose block declared it (listed in that class's `methods`).
    functions: HashMap<FunId, ClassId>,
    /// Static storage of block properties; its `IrStatic::owner` is the declaring class.
    storage: HashSet<u32>,
    properties: Vec<IrCompanionBlockProperty>,
    /// Block properties whose initializer is a metadata constant (`HAS_CONSTANT`).
    constant_initializers: HashSet<crate::fir::PropertyId>,
}

/// A `companion { … }` block property: a static member of `class`, realized by `storage` (a static
/// owned by that class) and/or accessor functions placed among the class's static methods.
#[derive(Clone, Debug)]
pub struct IrCompanionBlockProperty {
    pub class: TypeName,
    pub name: String,
    pub ty: Ty,
    pub is_var: bool,
    pub is_const: bool,
    pub has_constant: bool,
    pub visibility: crate::types::Visibility,
    pub storage: Option<u32>,
    pub getter: Option<FunId>,
    pub setter: Option<FunId>,
    pub source_order: u32,
}

impl IrCompanionBlocks {
    /// Record that `function`, listed among `owner`'s methods, was declared by `owner`'s block.
    pub fn place_function(&mut self, function: FunId, owner: ClassId) {
        self.functions.insert(function, owner);
    }

    /// The class whose block declared `function`: the static owner a same-file call names.
    pub fn declaring_class(&self, function: FunId) -> Option<ClassId> {
        self.functions.get(&function).copied()
    }

    pub fn is_function(&self, function: FunId) -> bool {
        self.functions.contains_key(&function)
    }

    pub fn has_functions(&self) -> bool {
        !self.functions.is_empty()
    }

    pub fn mark_storage(&mut self, index: u32) {
        self.storage.insert(index);
    }

    /// Whether static `index` stores a block property.
    pub fn is_storage(&self, index: u32) -> bool {
        self.storage.contains(&index)
    }

    pub fn record_property(&mut self, property: IrCompanionBlockProperty) {
        self.properties.push(property);
    }

    /// The block properties `class` declares, for its `@Metadata`.
    pub fn properties_of(
        &self,
        class: TypeName,
    ) -> impl Iterator<Item = &IrCompanionBlockProperty> {
        self.properties
            .iter()
            .filter(move |property| property.class == class)
    }

    pub fn mark_constant_initializer(&mut self, property: crate::fir::PropertyId) {
        self.constant_initializers.insert(property);
    }

    pub fn has_constant_initializer(&self, property: crate::fir::PropertyId) -> bool {
        self.constant_initializers.contains(&property)
    }
}

impl IrPackageProperty {
    /// Whether this is a written `companion val C.p`: its receiver selects the companion scope and is
    /// not a JVM accessor parameter.
    pub fn is_companion_extension(&self) -> bool {
        self.flags.has(crate::fir::DeclarationFlags::COMPANION)
    }
}

impl IrModuleProperty {
    /// The classifier whose `companion { … }` block declared this property: its static owner, named
    /// by the associated receiver.
    pub fn companion_block_owner(&self) -> Option<TypeName> {
        self.flags
            .has(crate::fir::DeclarationFlags::COMPANION_BLOCK_MEMBER)
            .then_some(self.extension_receiver)
            .flatten()
            .and_then(|receiver| receiver.non_null().obj_internal())
    }
}
