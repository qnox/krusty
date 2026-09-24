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
    pub(crate) unit_result_tail_forwards: &'a crate::jvm::suspend::UnitResultTailForwards,
    pub(crate) bridge_returns: &'a crate::jvm::bridge_return_adaptations::BridgeReturnAdaptations,
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
