//! Checked declarations and JVM-only pass products consumed by class emission.

use super::KotlinMetadata;
use crate::backend::BackendClassifierSource;

/// Metadata and physical suspend facts emitted beside one file's JVM classes.
pub(crate) struct EmitMetadata<'a> {
    pub(crate) facade: Option<&'a KotlinMetadata>,
    pub(crate) continuations: &'a crate::jvm::suspend::ContinuationMetadataMap,
    /// Suspend functions whose state machine this emission owns, because their only suspension is
    /// inside a body it splices. See `docs/JVM_INLINE_BEFORE_CPS.md`.
    pub(crate) emit_time_machines: &'a crate::jvm::suspend::EmitTimeMachines,
    /// Forwarded suspend returns requiring the target version's physical `Unit` adaptation.
    pub(crate) suspended_result_returns: &'a crate::jvm::suspend::SuspendedResultReturns,
    pub(crate) intrinsic_probe_continuations: &'a crate::jvm::suspend::IntrinsicProbeContinuations,
    pub(crate) bridge_adaptations: &'a crate::jvm::bridge_adaptations::BridgeAdaptations,
    pub(crate) function_argument_arrays:
        &'a crate::jvm::function_argument_arrays::FunctionArgumentArrays,
    /// The overrides whose primitive result is declared as its wrapper.
    pub(crate) override_results: &'a crate::jvm::override_results::OverrideResults,
    /// JVM entry guards selected from exact collection override edges.
    pub(crate) collection_method_entry_barriers:
        &'a crate::jvm::collection_barriers::MethodEntryBarriers,
}

/// Checked semantic declarations plus JVM-only realization facts consumed by class emission.
pub(crate) struct CheckedEmitFacts<'a> {
    pub(crate) metadata: EmitMetadata<'a>,
    pub(crate) signature_symbols: &'a dyn BackendClassifierSource,
    pub(crate) property_realizations: &'a crate::jvm::property_realizations::PropertyRealizations,
    pub(crate) property_reference_realizations:
        &'a crate::jvm::property_references::PropertyReferenceRealizations,
    pub(crate) default_call_operands: &'a crate::jvm::default_call_operands::DefaultCallOperands,
}
