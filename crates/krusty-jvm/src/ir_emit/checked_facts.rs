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
    pub(crate) source_member_annotations: &'a krusty::backend::SourceMemberAnnotations,
    pub(crate) dependency_callables: &'a krusty::backend::CheckedBackendCallables,
    pub(crate) property_realizations: &'a crate::property_realizations::PropertyRealizations,
    pub(crate) property_reference_realizations:
        &'a crate::property_references::PropertyReferenceRealizations,
    pub(crate) default_call_operands: &'a crate::default_call_operands::DefaultCallOperands,
    pub(crate) sam_wrapper_realizations: &'a crate::sam_wrappers::SamWrapperRealizations,
    pub(crate) local_delegate_access: &'a crate::local_delegate_accessors::HelperAccess,
}

impl<'a> CheckedEmitFacts<'a> {
    pub(super) fn environment(
        self,
        ir: &'a krusty::ir::IrFile,
        (facade_class, facade): (krusty::types::TypeName, &str),
        bodies: &'a dyn krusty::jvm::inline::MethodBodies,
        opts: &'a super::EmitOptions,
        run: &'a super::EmitRun,
    ) -> super::EmitEnv<'a> {
        super::EmitEnv {
            facade_class,
            multifile_facade: opts.multifile_facade.as_deref(),
            bodies,
            run,
            continuation_metadata: self.metadata.continuations,
            emit_time_machines: self.metadata.emit_time_machines,
            suspended_result_returns: self.metadata.suspended_result_returns,
            intrinsic_probe_continuations: self.metadata.intrinsic_probe_continuations,
            bridge_adaptations: self.metadata.bridge_adaptations,
            function_argument_arrays: self.metadata.function_argument_arrays,
            override_results: self.metadata.override_results,
            collection_method_entry_barriers: self.metadata.collection_method_entry_barriers,
            collection_members: self.metadata.collection_members,
            signature_symbols: self.signature_symbols,
            source_member_annotations: self.source_member_annotations,
            dependency_callables: self.dependency_callables,
            dispatch_classifiers: std::rc::Rc::new(
                crate::member_dispatch::CheckedDispatchClassifiers::new(ir, self.signature_symbols),
            ),
            jvm_default: opts.jvm_default,
            lambda_modes: opts.lambda_modes,
            java_parameters: opts.java_parameters,
            inline_string_concat: opts.inline_string_concat,
            inline_scopes_numbers: opts.inline_scopes_numbers,
            metadata_stamp: opts.metadata_stamp(),
            property_realizations: self.property_realizations,
            property_reference_realizations: self.property_reference_realizations,
            sam_wrapper_realizations: self.sam_wrapper_realizations,
            local_delegate_access: self.local_delegate_access,
            default_call_operands: self.default_call_operands,
            inner_classes: crate::inner_classes::InnerClasses::new(
                ir,
                self.metadata.override_results,
                facade,
                opts.lambda_modes,
            ),
        }
    }
}
