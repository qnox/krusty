//! The static owners of PRIVATE static declarations, and which private storage is reached through
//! its owner's synthetic accessors.
//!
//! A private top-level declaration is a static member of its file facade, and a private
//! `companion { … }` block member one of the class that declared the block. Only that class may
//! reach it; code the file emits into any other class goes through the owner's `access$…`
//! accessors. The owner travels as an identity from planning to emission and becomes text only in
//! a class-file constant.

use crate::ir::IrFile;
use crate::types::TypeName;

/// The JVM class a static declaration is a member of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StaticOwner {
    /// The file facade of the emission run.
    Facade,
    /// A class this file declares.
    Class(TypeName),
}

impl StaticOwner {
    /// The owner a static call or reference names, `None` naming the facade.
    pub(crate) fn of(owner: Option<TypeName>) -> Self {
        owner.map_or(Self::Facade, Self::Class)
    }

    /// Whether the owner is an interface: its static methods are then named by an
    /// `InterfaceMethodref`, and none of them may be `final`.
    pub(crate) fn is_interface(self, ir: &IrFile) -> bool {
        match self {
            Self::Facade => false,
            Self::Class(name) => ir
                .classes
                .iter()
                .any(|class| class.fq_name == name && class.is_interface),
        }
    }

    /// The owner's JVM internal name, for a class-file constant.
    pub(crate) fn internal_name(self, facade: &str) -> String {
        match self {
            Self::Facade => facade.to_string(),
            Self::Class(name) => name.render(),
        }
    }
}

/// Private static storage that code of any class but its owner reaches only through the owner's
/// `access$get<X>$p` / `access$set<X>$p` accessors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BridgedStorage {
    /// The storage's index in the file's static table.
    pub(crate) index: u32,
    pub(crate) owner: StaticOwner,
}

/// The selection, made here for every consumer: static storage `index` is bridged when it is a
/// PRIVATE plain field — no constant value, no accessor of its own, no `@JvmField` — of a file
/// facade or of the class whose `companion { … }` block declared it.
pub(crate) fn bridged_storage(ir: &IrFile, index: u32) -> Option<BridgedStorage> {
    let property = &ir.statics[index as usize];
    if property.is_const
        || property.custom_accessor
        || ir.is_jvm_field_static(index)
        || !property.visibility.is_private()
    {
        return None;
    }
    let owner = match property.owner {
        None => StaticOwner::Facade,
        Some(owner) if ir.companion_blocks.is_storage(index) => StaticOwner::Class(owner),
        Some(_) => return None,
    };
    Some(BridgedStorage { index, owner })
}
