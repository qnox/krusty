//! JVM class-file reading, writing, and bytecode emission (all JVM-specific code).

pub mod backend;
pub mod bridges;
pub mod classfile;
pub mod classpath;
pub mod classreader;
pub mod companion;
pub mod inline;
pub mod inline_class;
pub mod ir_emit;
pub mod java_stub;
pub mod jvm_class_map;
pub mod jvm_libraries;
pub mod metadata;
pub mod names;
pub mod suspend;
pub mod value_classes;

pub use backend::{prepare_module_symbols, JvmBackend};

/// Kotlin distribution artifacts used by JVM command-line defaults.
pub fn kotlin_stdlib_jar() -> Option<std::path::PathBuf> {
    crate::toolchain::stdlib_jar()
}

pub fn kotlin_dist_jar(name: &str) -> Option<std::path::PathBuf> {
    crate::toolchain::dist_jar(name)
}
