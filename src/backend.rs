//! Backend contracts.
//!
//! A backend consumes checked frontend output and emits target artifacts.

pub(crate) mod class_tables;
pub(crate) mod coroutines;
pub(crate) mod counted_loops;
mod dependency_accessor;
mod dependency_call_shape;
mod dependency_facts;
mod dependency_metadata;
mod dependency_signature;
mod entry;
pub(crate) mod interface_identity;
pub(crate) mod local_properties;
mod module_facts;

pub(crate) use dependency_call_shape::{DependencyCallShape, DependencyExportCall};
#[cfg(test)]
pub(crate) use dependency_facts::referenced_dependencies;
pub use dependency_facts::{
    BackendCallableFact, BackendCompilerIntrinsic, BackendPropertyFact, BackendSemanticCallRole,
    CheckedBackendCallables, DependencyClassifier, DependencyFactError, DependencyGetterSignature,
};
pub(crate) use dependency_metadata::KlibDependencyCallable;
pub(crate) use dependency_metadata::{
    authenticated_singleton_receiver, has_standalone_dependency_code,
    inert_staged_singleton_bindings,
};
pub use dependency_metadata::{
    DependencyExportSignatures, DependencyMemberReceiver, DependencyMetadata,
    SingletonExtensionUnreadReceiver, SingletonMemberReceiver,
};
pub(crate) use dependency_signature::{
    instantiate_dependency_signature, DependencyFunctionSignature,
};

/// The parameter form [`Entry::selected`] reports beside the selected function.
pub(crate) use crate::ir::MainEntryParameters;
pub use entry::{Entry, BOX_RESULT_FRAME};

pub use module_facts::{
    BackendClassifierFact, BackendClassifierSource, BackendFactError, BackendModuleFacts,
    CheckedBackendClassifiers, EnumEntriesAccessor, UndeterminedType,
};

use crate::compilation_target::CompilationTarget;
use crate::diag::DiagSink;
use crate::ir::IrFile;

/// Whether a checked IR unit is a source file or compiler-owned dependency support code.
///
/// Support code is closed over its internal calls and publishes only the exact dependency exports
/// recorded beside it. Backends must not expose its declaration spellings through their ordinary
/// source ABI or let those internal spellings enter a process-wide symbol namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckedIrUnitKind {
    Source,
    DependencySupport,
}

/// One streamed common-IR unit plus the compact module facts a backend may use for physical
/// realization. No syntax from another source unit is reachable through this view.
pub struct CheckedIrFile<'a> {
    pub ir: IrFile,
    pub kind: CheckedIrUnitKind,
    pub source: crate::fir::SourceFileId,
    /// Frozen classifier facts. Callables and properties are already selected in checked IR;
    /// exposing the frontend symbol table here would permit lookup and provisional local
    /// signatures to leak across the backend boundary.
    pub classifiers: CheckedBackendClassifiers<'a>,
    /// Frozen facts for exactly the dependency callables and properties this file's IR selected,
    /// copied from their provider at this boundary so emission need not ask the provider about one
    /// again.
    pub callables: CheckedBackendCallables,
    /// Exact KLIB declaration identities exported by this source module, keyed by the common-IR
    /// callable that realizes each declaration. A dependency-metadata sink records these before a
    /// target mutates the IR, so target code and a later module's `Callee::External` join through
    /// one stable identity rather than a source or physical spelling.
    pub dependency_exports: DependencyExportSignatures,
    /// The native compiler plugins the frontend ran for this compilation. A backend runs exactly
    /// these, so a declaration the frontend published is realized and no unrequested one appears.
    pub native_plugins: &'a crate::plugins::registry::NativePlugins,
    pub module_name: &'a str,
    pub stems: &'a [String],
    /// Where each checked node came from. A backend that declines a construct reports it at the
    /// source span of the node it declined, which common IR identifies by origin.
    pub origins: &'a crate::fir::OriginStore,
}

impl CheckedIrFile<'_> {
    /// The source file (by index into the module's sources) and span a checked node came from.
    pub fn origin_span(&self, origin: crate::fir::OriginId) -> Option<(u32, crate::diag::Span)> {
        self.origins
            .source_span(origin)
            .map(|(source, span)| (source.raw(), span))
    }
}

/// One emitted artifact: a target-relative path and its bytes (e.g. `Foo.class`, a `.wasm` module).
pub type Artifact = (String, Vec<u8>);

pub trait Backend {
    /// Cross-file state accumulated while lowering.
    type State: Default;

    /// The platform this backend emits for. The analysis it consumes must have been checked under
    /// the same platform's rules.
    fn compilation_target(&self) -> CompilationTarget;

    /// Whether this backend emits compiler-owned executable bodies selected from dependencies.
    /// Metadata writers leave this disabled: their output describes the source module only.
    fn emits_dependency_support(&self) -> bool {
        false
    }

    /// Whether this backend numbers the dispatch slots of the module's classes itself, so a file
    /// needs the module's class shapes ([`BackendModuleFacts::with_classes`]) to dispatch through
    /// a class another file declares.
    fn numbers_slots(&self) -> bool {
        false
    }

    /// Whether a dependency-support body may call an exact provider declaration which has no
    /// serialized KLIB body. Such a call is admitted only when the target's ordinary dependency
    /// registry can realize the frozen provider handle directly.
    fn realizes_bodyless_dependency_support(&self) -> bool {
        false
    }

    /// A positive target implementation of a provider-authenticated declaration. This does not
    /// authorize common folding or replace a retained dependency export.
    fn realizes_dependency_intrinsic(&self, _operation: BackendCompilerIntrinsic) -> bool {
        false
    }

    /// A positive target implementation of a complete authenticated getter declaration. This
    /// never authorizes an ordinary class body or displaces a retained dependency export.
    fn realizes_dependency_getter(&self, _signature: DependencyGetterSignature<'_>) -> bool {
        false
    }

    /// Whether the target, not the library's serialized class body, owns the representation of a
    /// dependency classifier. A member of such a classifier is realized by the target at its
    /// dispatch receiver, as a source call of it is; its KLIB body reads a class layout the target
    /// object does not have and is never lowered.
    fn represents_dependency_classifier(&self, _classifier: DependencyClassifier) -> bool {
        false
    }

    /// How common lowering realizes calls to same-module inline functions for this backend. A
    /// backend that compiles each inline function's body and inlines it at the call keeps the calls
    /// it can inline that way; every other backend has them expanded in IR.
    fn same_module_inlining(&self) -> crate::ir::SameModuleInlining {
        crate::ir::SameModuleInlining::ExpandInIr
    }

    /// Consume one checked common-IR file produced by the streaming FIR path. No parsed source or
    /// AST-keyed semantic table crosses this boundary; target realization consumes only checked IR
    /// and compact stable module facts.
    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact>;

    /// Report what makes the accumulated module unemittable as a whole (a program with no entry,
    /// say) before anything is finalized. An error here suppresses [`Backend::finalize`].
    fn check_module(&self, _state: &Self::State, _diags: &mut DiagSink) {}

    /// Emit any whole-module artifacts from the accumulated `state` (e.g. `META-INF/<m>.kotlin_module`).
    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact>;
}
