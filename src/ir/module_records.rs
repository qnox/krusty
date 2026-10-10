//! Current-module declarations a file references, as common lowering copies them.
//!
//! Checked FIR keeps stable identities while bodies stream. Before a file reaches a backend, the
//! exact semantic facts each referenced module declaration needs are copied into these records, so
//! a target realizes the edge without reopening the frontend module index.

use super::{IrParameterIdentity, IrStaticPlacement, Ty, TypeName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrModuleSource {
    pub source: crate::fir::SourceFileId,
    pub package: TypeName,
}

/// One declaration type parameter copied with a referenced module callable. Only the semantic
/// name and bounds participate in JVM class-bound erasure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrCallableTypeParameter {
    pub semantic_name: String,
    pub bounds: Box<[(Ty, bool)]>,
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
    /// Final Kotlin declaration visibility. A target may need it to realize access from a
    /// physically separate nested/generated class, but must not recover it from an emitted name or
    /// owner.
    pub visibility: crate::types::Visibility,
    /// Final source declaration flags needed after stable module calls cross into target realization.
    pub flags: crate::fir::DeclarationFlags,
    /// Final declaration signature, including context and extension receiver parameters but never
    /// a target-specific dispatch receiver, continuation, default mask, or marker. Backends use it
    /// for representation ABI without reopening FIR or reverse-engineering a synthetic descriptor.
    pub parameters: Box<[Ty]>,
    /// The positions in `parameters` that declare a default value.
    pub default_parameters: Box<[usize]>,
    /// Declaration type parameters and their complete bounds. A JVM realization erases a call to
    /// this declaration with the same primary class bound it uses for a same-file signature.
    pub type_parameters: Box<[IrCallableTypeParameter]>,
    /// Stable source/generated identities parallel to `parameters`. Backends use these for debug
    /// and parameter metadata on synthesized adapters instead of inventing positional names.
    pub parameter_identities: Box<[IrParameterIdentity]>,
    pub result: Ty,
    /// Resolved declaration annotations with only the compact constant-string payload needed by
    /// target realization. No source spelling, expression, or parser coordinate survives here.
    pub annotations: Box<[IrHeaderAnnotation]>,
    /// Where the function lives when `owner` is absent.
    pub placement: IrStaticPlacement,
    /// The own, unapplied results of the declarations this one overrides, as the override edges its
    /// classifier published them. A target whose realization of an override's result depends on
    /// the overridden results realizes a call to it, or an override of it, from these in every file.
    pub overridden_results: Box<[Ty]>,
}

/// Exact current-module declaration selected for a member operation, plus the selected semantic
/// parameter shape at that use site. The stable declaration identity remains authoritative for
/// visibility/ownership; the selected parameters let a backend build a representation adapter for
/// a generic call without reverse-engineering types from operands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrModuleMemberAccess {
    Callable {
        target: crate::fir::CallableId,
        selected_parameters: Box<[Ty]>,
    },
    Property {
        target: crate::fir::PropertyId,
        write: bool,
        selected_parameters: Box<[Ty]>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrHeaderAnnotation {
    pub identity: TypeName,
    pub string_arguments: Box<[Box<str>]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrClassifierKind {
    Class,
    Interface,
    Annotation,
    Enum,
    Object,
}

/// A module constructor as every file that names it sees it. The file that declares the class
/// and each file that constructs it copy this record from one checked declaration, so a target
/// can realize both sides of the call from the same facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrModuleConstructor {
    /// The constructed classifier's stable qualified identity.
    pub owner: TypeName,
    /// The constructed classifier's declaration flags: its kind, `abstract`/`sealed`, `inner`,
    /// local, `expect`, and value-class shape.
    pub owner_flags: crate::fir::DeclarationFlags,
    pub flags: crate::fir::DeclarationFlags,
    pub visibility: crate::types::Visibility,
    pub context_parameter_count: u32,
    /// The checked declaration signature, context parameters first, each at its stored value type.
    /// An inner class's enclosing instance is not a declared parameter; `outer` names it.
    pub parameters: Box<[Ty]>,
    /// An inner class's enclosing classifier, whose instance a construction passes first.
    pub outer: Option<TypeName>,
}

/// The module constructors a file names: what each construction selected, and the record of
/// every module constructor that file constructs or declares.
#[derive(Clone, Debug, Default)]
pub struct IrModuleConstructions {
    /// Construction `ExprId` → the checked constructor declaration it selected.
    pub selected: std::collections::HashMap<super::ExprId, crate::fir::DeclarationId>,
    pub records: std::collections::HashMap<crate::fir::DeclarationId, IrModuleConstructor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrModuleClassifier {
    pub singleton: bool,
    pub companion_owner: Option<TypeName>,
    /// Source kind of this classifier. A property reference calls an inherited member on the
    /// classifier it was written on, and that kind chooses interface dispatch.
    pub kind: IrClassifierKind,
}

impl super::IrFile {
    /// Source classifier kind for an exact common-IR identity, regardless of whether the
    /// declaration belongs to this file or another file in the module.
    pub(crate) fn source_classifier_kind(&self, classifier: TypeName) -> Option<IrClassifierKind> {
        if let Some(class) = self
            .classes
            .iter()
            .find(|candidate| candidate.fq_name_id() == classifier)
        {
            return Some(if class.is_annotation {
                IrClassifierKind::Annotation
            } else if class.is_object {
                IrClassifierKind::Object
            } else if class.is_enum {
                IrClassifierKind::Enum
            } else if class.is_interface {
                IrClassifierKind::Interface
            } else {
                IrClassifierKind::Class
            });
        }
        self.referenced_module_classifiers
            .get(&classifier)
            .map(|classifier| classifier.kind)
    }
}
