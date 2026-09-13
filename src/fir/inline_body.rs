//! Checked, source-independent inline expansion contracts.

use super::body::FirIteratorCall;
use super::identities::ExternalCallableId;
use super::signature::ResolvedTy;

/// Parameter-relative value selected by a checked inline plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirInlineValue {
    Receiver,
    Parameter(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirInlineBodyPlan {
    InvokeLambda {
        lambda_parameter: u32,
        arguments: Box<[FirInlineValue]>,
        result: Option<FirInlineValue>,
    },
    /// Declaration-scoped iterator expansion for the exact selected inline `forEach` declaration.
    /// All three convention calls were selected by the checker at the call site; lowering only
    /// splices the checked lambda body into the resulting loop.
    ForEach {
        lambda_parameter: u32,
        iterator_ty: ResolvedTy,
        iterator: Box<FirIteratorCall>,
        has_next: Box<FirIteratorCall>,
        next: Box<FirIteratorCall>,
    },
    /// Checked structural expansion of an exact collection inline declaration. Iterator convention
    /// calls were selected in the declaration's lookup scope; factory/append are opaque provider
    /// identities. Common lowering therefore performs no library lookup or target-ABI reasoning.
    CollectionTransform {
        lambda_parameter: u32,
        flatten: bool,
        local_names: FirInlineCollectionLocalNames,
        iterator_ty: ResolvedTy,
        iterator: Box<FirIteratorCall>,
        has_next: Box<FirIteratorCall>,
        next: Box<FirIteratorCall>,
        factory: ExternalCallableId,
        factory_classifier: crate::types::TypeName,
        append: ExternalCallableId,
        accumulator: ResolvedTy,
        append_parameter: ResolvedTy,
        append_result: ResolvedTy,
    },
    /// Inline a declaration whose checked body enters a suspending region, invokes a zero-argument
    /// lambda, and leaves the region from `finally`. The selected member identities are opaque;
    /// target-specific owners, descriptors, and invocation opcodes remain provider/backend data.
    SuspendBeforeLambdaFinally {
        lambda_parameter: u32,
        /// The optional argument both members take besides the receiver, with the value the
        /// declaration's own default supplies when the caller omits it. `withLock` threads its
        /// `owner`; `withPermit` threads nothing.
        state: Option<FirInlineBodyState>,
        enter: FirInlineMemberCall,
        cleanup: FirInlineMemberCall,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineBodyState {
    pub parameter: u32,
    pub default: FirInlineDefaultValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineCollectionLocalNames {
    pub outer_receiver: Box<str>,
    pub inner_receiver: Box<str>,
    pub destination: Box<str>,
    pub element: Box<str>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineMemberCall {
    pub declaration: ExternalCallableId,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub suspend: bool,
}
