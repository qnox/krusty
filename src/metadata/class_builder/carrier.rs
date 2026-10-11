//! What a carrier adds to the `Class` record the schema lays out.
//!
//! The record's own fields are the same wherever it is stored. A carrier numbers the annotation
//! fields and adds its own extensions; each hook is called where kotlinc interns that extension's
//! strings, so a carrier never has to reorder the string table.

use crate::metadata::protobuf::Pb;
use crate::metadata::type_encoder::{StringTable, TypeParameters};

use super::declarations::FnMeta;

/// A declaration an annotation is applied to, for [`ClassCarrier::annotation_field`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnnotationSite {
    Class,
    Constructor,
    ValueParameter,
    EnumEntry,
    Function,
    Property,
    Getter,
    Setter,
    BackingField,
}

/// A constructor of the declaration, for [`ClassCarrier::constructor_extensions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConstructorSlot {
    Primary,
    /// The declaration's secondary constructor at this index.
    Secondary(usize),
}

pub(crate) trait ClassCarrier {
    /// The field the annotations applied at `site` are written to.
    fn annotation_field(&self, site: AnnotationSite) -> u32;

    /// A constructor's extensions, interned before its annotations.
    fn constructor_extensions(&self, st: &mut StringTable<'_>, constructor: ConstructorSlot) -> Pb;

    /// Property `index`'s extensions interned before its annotations; `index` is its position in
    /// the declaration's properties.
    fn property_extensions(&self, st: &mut StringTable<'_>, index: usize) -> Pb;

    /// Property `index`'s extensions interned after its annotations.
    fn property_trailer(&self, st: &mut StringTable<'_>, index: usize) -> Pb;

    /// Function `index`'s extensions interned before its annotations; `index` is its position in
    /// the declaration's functions.
    fn function_extensions(&self, st: &mut StringTable<'_>, index: usize) -> Pb;

    /// A function's extensions interned after everything else in it.
    fn function_trailer(&self, st: &mut StringTable<'_>, function: &FnMeta) -> Pb;

    /// The class's extensions interned after its structural strings and before its annotations.
    fn class_extensions(&self, st: &mut StringTable<'_>, type_parameters: &TypeParameters) -> Pb;

    /// The class's extensions interned last of all.
    fn class_trailer(&self, st: &mut StringTable<'_>) -> Pb;
}
