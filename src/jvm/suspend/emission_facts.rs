//! JVM emission facts produced while coroutine lowering still owns the physical suspend shape.

/// JVM class metadata computed before continuation spill scopes are discarded.
#[derive(Clone, Debug, Default)]
pub struct ContinuationMetadata {
    pub l: Vec<i32>,
    pub nl: Vec<i32>,
    pub i: Vec<i32>,
    pub s: Vec<String>,
    pub n: Vec<String>,
    pub m: String,
    pub c: String,
    pub v: i32,
    pub enclosing_class: String,
    pub enclosing_method: String,
    pub enclosing_descriptor: String,
}

pub type ContinuationMetadataMap = std::collections::HashMap<String, ContinuationMetadata>;

/// What a JVM return node makes of the suspend result it returns when that result is not
/// `COROUTINE_SUSPENDED`, which it returns as is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SuspendedResultReturn {
    /// A forwarded `Unit` result becomes `Unit.INSTANCE`, for the selected Kotlin target version.
    Unit,
    /// A value class's carrier, as a function returns it, becomes the value class's box, as a
    /// continuation's `invokeSuspend` hands it to its completion. A nullable carrier boxes all but
    /// `null`.
    ValueClassBox {
        classifier: crate::types::TypeName,
        carrier: crate::types::Ty,
    },
    /// An interface-delegation forwarder whose callee returns a value class as its reference
    /// carrier. `COROUTINE_SUSPENDED` is returned as it is; any other result is that carrier,
    /// which `checkcast`s and is returned without boxing.
    ValueClassCarrier { carrier: crate::types::Ty },
}

/// The return nodes that reshape a suspend result, and how.
pub(crate) type SuspendedResultReturns =
    std::collections::HashMap<crate::ir::ExprId, SuspendedResultReturn>;

/// JVM value indices holding the continuation passed to an unintercepted intrinsic suspension
/// point. The CPS pass owns this physical choice; common IR retains only the semantic point kind.
pub(crate) type IntrinsicProbeContinuations = std::collections::HashMap<crate::ir::ExprId, u32>;

/// What building a file's machines records for their emission.
pub(super) struct MachineOutputs<'b> {
    pub(super) continuation_metadata: &'b mut ContinuationMetadataMap,
    pub(super) default_call_operands:
        &'b mut crate::jvm::default_call_operands::DefaultCallOperands,
    pub(super) suspended_result_returns: &'b mut SuspendedResultReturns,
    pub(super) intrinsic_probe_continuations: &'b mut IntrinsicProbeContinuations,
}

/// The function an IR state machine is built for: its body, whether it returns `Unit`, the
/// suspension scopes captured before inline bodies were spliced, and its suspension lines.
pub(super) struct MachineSubject<'a> {
    pub(super) fid: u32,
    pub(super) body: crate::ir::ExprId,
    pub(super) unit_ret: bool,
    pub(super) captured_scopes: Option<super::SuspensionScopes>,
    pub(super) suspension_lines: &'a std::collections::HashMap<crate::ir::ExprId, (u32, u32)>,
}
