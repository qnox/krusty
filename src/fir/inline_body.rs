//! Checked, source-independent inline expansion contracts.

use super::identities::ExternalCallableId;
use super::signature::ResolvedTy;

/// Parameter-relative value selected by a checked inline plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirInlineValue {
    Receiver,
    Parameter(u32),
    /// Throwable escaping the lambda, or `null` on its normal exit.
    Cause,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirInlineCallReceiver {
    Dispatch(FirInlineValue),
    Extension(FirInlineValue),
}

/// Checked index behavior of a declaration-owned iteration body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirInlineIterationIndex {
    Unchecked,
    Checked { overflow: Box<FirInlineCall> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineIterationMemberCall {
    pub declaration: ExternalCallableId,
    pub receiver: ResolvedTy,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    /// The declaration's ABI result before call-site substitution (a JVM descriptor's erasure):
    /// the type the inlined frame's raw locals hold, where `result` is the applied selection.
    pub physical_result: ResolvedTy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirInlineIterationTraversal {
    Iterator {
        prepare: Box<[FirInlineIterationMemberCall]>,
        has_next: Box<FirInlineIterationMemberCall>,
        next: Box<FirInlineIterationMemberCall>,
    },
    Array,
    Counted {
        size: Box<FirInlineIterationMemberCall>,
        get: Box<FirInlineIterationMemberCall>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineCall {
    pub declaration: ExternalCallableId,
    /// Context and source value parameters. An extension receiver remains the explicit semantic
    /// receiver above and is not duplicated in this list.
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub suspend: bool,
    pub receiver: Option<FirInlineCallReceiver>,
    pub arguments: Box<[FirInlineValue]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirInlineDefault {
    pub parameter: u32,
    pub value: FirInlineDefaultValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineRecovery {
    pub caught: ResolvedTy,
    pub constructor: ExternalCallableId,
    pub classifier: crate::types::TypeName,
    pub constructor_parameters: Box<[ResolvedTy]>,
    pub failure: Box<FirInlineCall>,
}

/// Checked frame facts of an exact inline declaration: how its expansion's debug surface is
/// spelled. See [`crate::libraries::InlineBodyFrame`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineBodyFrame {
    /// Simple source name of the declaration, spelling `$i$f$`/`$i$a$` frame markers.
    pub callee: Box<str>,
    /// An `@InlineOnly` declaration contributes no function marker, no named frame locals, and no
    /// body line mappings: the expansion takes the call site's lines.
    pub inline_only: bool,
    /// The declaration body's resolved debug surface; `None` when inline-only.
    pub source: Option<FirInlineBodySource>,
}

/// Resolved debug-source identity of a non-inline-only declaration body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineBodySource {
    /// Source file the body's lines belong to and the path they map under.
    pub file: Box<str>,
    pub path: Box<str>,
    /// Resolved source line of the body's lambda invocation and of its frame's close.
    pub invoke_line: u16,
    pub close_line: u16,
    /// Source name of the body's iteration element local, when it declares one.
    pub element: Option<Box<str>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirInlineBodyPlan {
    InvokeLambda {
        frame: FirInlineBodyFrame,
        lambda_parameter: u32,
        arguments: Box<[FirInlineValue]>,
        prologue: Box<[FirInlineCall]>,
        cleanup: Box<[FirInlineCall]>,
        /// Nullable semantic throwable type recorded by the checked catch template.
        cause: Option<ResolvedTy>,
        recovery: Option<Box<FirInlineRecovery>>,
        defaults: Box<[FirInlineDefault]>,
        result: Option<FirInlineValue>,
        /// Declared extension receiver when it is a type parameter. The expansion stores that
        /// parameter as its erased bound; an unconstrained parameter is absent because its bound
        /// erases to `Object` and the specialized reference stays.
        declared_receiver: Option<ResolvedTy>,
    },
    /// Declaration-scoped iterator expansion for the exact selected inline `forEach` declaration.
    /// All three convention calls were selected by the checker at the call site; lowering only
    /// splices the checked lambda body into the resulting loop.
    Iteration {
        frame: FirInlineBodyFrame,
        lambda_parameter: u32,
        element: ResolvedTy,
        index: Option<FirInlineIterationIndex>,
        traversal: FirInlineIterationTraversal,
    },
    /// Checked structural expansion of an exact collection inline declaration. Traversal,
    /// factory, and append are opaque provider identities. Common lowering therefore performs no
    /// library lookup or target-ABI reasoning.
    CollectionTransform {
        lambda_parameter: u32,
        local_names: FirInlineCollectionLocalNames,
        traversal: FirInlineIterationTraversal,
        factory: ExternalCallableId,
        factory_classifier: crate::types::TypeName,
        factory_parameters: Box<[ResolvedTy]>,
        capacity: Option<FirInlineCollectionCapacity>,
        append: Box<FirInlineCollectionAppend>,
        accumulator: ResolvedTy,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineCollectionLocalNames {
    pub outer_receiver: Box<str>,
    pub inner_receiver: Box<str>,
    pub destination: Box<str>,
    pub element: Box<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirInlineCollectionCapacity {
    Member(FirInlineIterationMemberCall),
    Extension {
        call: FirInlineCollectionExtensionCall,
        default: i32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirInlineCollectionAppend {
    Member(FirInlineIterationMemberCall),
    Extension(FirInlineCollectionExtensionCall),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineCollectionExtensionCall {
    pub declaration: ExternalCallableId,
    pub source_receiver: ResolvedTy,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
}

impl From<&crate::libraries::InlineBodyFrame> for FirInlineBodyFrame {
    fn from(frame: &crate::libraries::InlineBodyFrame) -> Self {
        Self {
            callee: frame.callee.clone(),
            inline_only: frame.inline_only,
            source: frame.source.as_ref().map(|source| FirInlineBodySource {
                file: source.file.clone(),
                path: source.path.clone(),
                invoke_line: source.invoke_line,
                close_line: source.close_line,
                element: source.element.clone(),
            }),
        }
    }
}

impl From<&crate::libraries::InlineCollectionLocalNames> for FirInlineCollectionLocalNames {
    fn from(names: &crate::libraries::InlineCollectionLocalNames) -> Self {
        Self {
            outer_receiver: names.outer_receiver.clone(),
            inner_receiver: names.inner_receiver.clone(),
            destination: names.destination.clone(),
            element: names.element.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirInlineDefaultValue {
    Null,
}
