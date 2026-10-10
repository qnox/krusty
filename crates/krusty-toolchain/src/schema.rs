//! The shape of what module files may say: every property, its type and its default, transcribed
//! from the toolchain's schema (`frontend-api/.../schema`). Readers check values against it, the
//! merge fills defaults from it, and `show settings` prints it.

mod dependencies;
mod module_file;
mod settings;
mod types;

pub use module_file::{FRAGMENT, MODULE, TEMPLATE};
pub use types::{
    render, Default, DependencyKind, Derivation, EnumType, ObjectType, Property, Type,
};

/// The object types a dependency is read as, for the reader's variant choice.
pub mod dependency_types {
    pub use super::dependencies::{
        BOM, CATALOG, EXTERNAL_MAVEN, INTERNAL, UNSCOPED_BOM, UNSCOPED_CATALOG,
        UNSCOPED_EXTERNAL_MAVEN, UNSCOPED_MODULE,
    };
}
