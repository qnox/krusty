//! JVM class-file reading, writing, and bytecode emission (all JVM-specific code).

pub mod backend;
pub mod classfile;
pub mod classpath;
pub mod classreader;
pub mod inline;
pub mod inline_class;
pub mod ir_emit;
pub mod jvm_class_map;
pub mod jvm_libraries;
pub mod metadata;
pub mod names;
pub mod suspend;
pub mod value_classes;

pub use backend::{prepare_module_symbols, JvmBackend};
