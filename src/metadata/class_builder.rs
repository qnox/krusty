//! Build the `@kotlin.Metadata` `d1`/`d2` payload for a Kotlin *class* (kind=1), so a Kotlin
//! consumer recognizes krusty's emitted class as a genuine Kotlin class (property syntax, etc.).
//!
//! Schema reverse-engineered from kotlinc 1.9.24 for `class Point(val x: Int, var y: String)`
//! (see METADATA_NOTES.md). `ProtoBuf.Class`: f3=fq_name (a class-id string-table entry),
//! f6=supertype `Type`, f8=constructor, f10=property (repeated). `Type.class_name`=f6.
//! `Constructor`: f2=value_parameter, f100=JvmMethodSignature ext (desc). `Property`: f2=name,
//! f3=return_type, f11=flags (emitted as 1798 only for a `var`), f100=JvmPropertySignature
//! {f1=field (empty → derived), f3=getter, f4=setter}. `JvmMethodSignature`: f1=name, f2=desc.
//!
//! String table: a class id uses operation `DESC_TO_CLASS_ID` (Record.f3=2) over `Lpkg/Name;`;
//! builtin types use `predefined_index` (Record.f2); everything else is a verbatim d2 entry.
//!
//! The `Class` record's fields are the same in every carrier; what a carrier adds (the JVM's
//! signatures and module name, a KLIB's file and constant extensions) and where it numbers the
//! annotation fields is a [`ClassCarrier`]. The record is laid out by [`schema`]; [`jvm`] and
//! [`klib`] are the two carriers.

mod carrier;
mod declarations;
mod jvm;
mod klib;
mod schema;
#[cfg(test)]
mod tests;

pub use declarations::*;
pub use jvm::{
    build_class, JvmClassSignatures, JvmConstructorSignature, JvmFieldSignature,
    JvmFunctionSignature, JvmPropertySignature,
};
pub(crate) use klib::KlibCarrier;
pub(crate) use schema::{
    append_param_annotations, class_message, param_annotation_flags, ClassDeclaration,
};
