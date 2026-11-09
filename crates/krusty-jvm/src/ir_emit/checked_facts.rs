//! Checked declarations and JVM-only pass products consumed by class emission.

use super::KotlinMetadata;
use krusty::backend::BackendClassifierSource;

/// Metadata and physical suspend facts emitted beside one file's JVM classes.
pub(crate) struct EmitMetadata<'a> {
    pub(crate) facade: Option<&'a KotlinMetadata>,
    pub(crate) continuations: &'a crate::suspend::ContinuationMetadataMap,
    /// Suspend functions whose state machine this emission owns, because their only suspension is
    /// inside a body it splices.
    pub(crate) emit_time_machines: &'a crate::suspend::EmitTimeMachines,
    /// Forwarded suspend returns requiring the target version's physical `Unit` adaptation.
    pub(crate) suspended_result_returns: &'a crate::suspend::SuspendedResultReturns,
    pub(crate) intrinsic_probe_continuations: &'a crate::suspend::IntrinsicProbeContinuations,
    pub(crate) bridge_adaptations: &'a crate::bridge_adaptations::BridgeAdaptations,
    pub(crate) function_argument_arrays:
        &'a crate::function_argument_arrays::FunctionArgumentArrays,
    /// The overrides whose primitive result is declared as its wrapper.
    pub(crate) override_results: &'a crate::override_results::OverrideResults,
    /// JVM entry guards selected from exact collection override edges.
    pub(crate) collection_method_entry_barriers:
        &'a crate::collection_barriers::MethodEntryBarriers,
    /// JVM-only collection member plans (inherited Java bridges, read-only stubs), keyed by
    /// emitted class identity.
    pub(crate) collection_members: &'a crate::read_only_collection_stubs::CollectionMemberPlans,
}

/// Checked semantic declarations plus JVM-only realization facts consumed by class emission.
pub(crate) struct CheckedEmitFacts<'a> {
    pub(crate) metadata: EmitMetadata<'a>,
    pub(crate) signature_symbols: &'a dyn BackendClassifierSource,
    pub(crate) dependency_callables: &'a krusty::backend::CheckedBackendCallables,
    pub(crate) property_realizations: &'a crate::property_realizations::PropertyRealizations,
    pub(crate) property_reference_realizations:
        &'a crate::property_references::PropertyReferenceRealizations,
    pub(crate) default_call_operands: &'a crate::default_call_operands::DefaultCallOperands,
    pub(crate) sam_wrapper_realizations: &'a crate::sam_wrappers::SamWrapperRealizations,
    pub(crate) local_delegate_access: &'a crate::local_delegate_accessors::HelperAccess,
}
