//! Current-module declarations a file references, as common lowering copies them.
//!
//! Checked FIR keeps stable identities while bodies stream. Before a file reaches a backend, the
//! exact semantic facts each referenced module declaration needs are copied into these records, so
//! a target realizes the edge without reopening the frontend module index.

use super::{IrStaticPlacement, Ty, TypeName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrModuleSource {
    pub source: crate::fir::SourceFileId,
    pub package: TypeName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrModuleCallable {
    pub source: IrModuleSource,
    /// Kotlin declaration name used for callable-reference identity. A target-specific annotation
    /// may change the emitted method name without changing this semantic spelling.
    pub name: Box<str>,
    /// Declaring classifier for a member `$default` bridge, and its source-level kind (a target
    /// decides what the kind means physically). Ordinary member calls are virtual/special calls.
    pub owner: Option<TypeName>,
    pub owner_kind: Option<IrClassifierKind>,
    /// Final source declaration flags needed after stable module calls cross into target realization.
    pub flags: crate::fir::DeclarationFlags,
    /// Final declaration signature, including context and extension receiver parameters but never
    /// a target-specific dispatch receiver, continuation, default mask, or marker. Backends use it
    /// for representation ABI without reopening FIR or reverse-engineering a synthetic descriptor.
    pub parameters: Box<[Ty]>,
    pub result: Ty,
    /// Resolved declaration annotations with only the compact constant-string payload needed by
    /// target realization. No source spelling, expression, or parser coordinate survives here.
    pub annotations: Box<[IrHeaderAnnotation]>,
    /// Where the function lives when `owner` is absent.
    pub placement: IrStaticPlacement,
    /// The declaration overrides one whose own result is not a Kotlin primitive, as the override
    /// edges its classifier published say (see [`super::IrFunctionOverride::overrides_non_primitive_result`]).
    /// A target that realizes such an override's primitive result differently realizes a call to it,
    /// or an override of it, from this same fact in every file.
    pub overrides_non_primitive_result: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrHeaderAnnotation {
    pub identity: TypeName,
    pub string_arguments: Box<[Box<str>]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrModuleProperty {
    pub source: IrModuleSource,
    pub name: String,
    pub ty: Ty,
    pub context_parameters: Vec<Ty>,
    pub extension_receiver: Option<Ty>,
    pub mutable: bool,
    pub owner: Option<TypeName>,
    /// Source-level kind of `owner`. Common IR retains the Kotlin declaration fact; a target backend
    /// decides whether that kind uses interface dispatch, singleton storage, or another physical form.
    pub owner_kind: Option<IrClassifierKind>,
    pub companion_associated: bool,
    /// Outer classifier whose companion object owns this declaration. This is the Kotlin
    /// singleton-association edge; it says nothing about target storage.
    pub companion_owner: Option<TypeName>,
    pub visibility: crate::types::Visibility,
    pub setter_is_private: bool,
    /// Resolved Kotlin annotation identities. A target backend may interpret annotations in its
    /// namespace; common lowering never turns one into a physical access kind.
    pub annotations: Box<[TypeName]>,
    pub flags: crate::fir::DeclarationFlags,
    /// Where the property lives when `owner` is absent.
    pub placement: IrStaticPlacement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrClassifierKind {
    Class,
    Interface,
    Annotation,
    Enum,
    Object,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrModuleClassifier {
    pub singleton: bool,
    pub companion_owner: Option<TypeName>,
}
