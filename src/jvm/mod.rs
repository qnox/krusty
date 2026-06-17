//! JVM class-file reading, writing, and bytecode emission (all JVM-specific code).

pub mod classreader;
pub mod classpath;
pub mod jvm_class_map;
pub mod classfile;
pub mod names;
pub mod ir_emit;
pub mod backend;

pub use backend::JvmBackend;
