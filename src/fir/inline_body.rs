//! Checked, source-independent inline expansion contracts.

use super::body::FirIteratorCall;
use super::identities::ExternalCallableId;
use super::signature::ResolvedTy;

/// Parameter-relative value selected by a checked inline plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirInlineValue {
    Receiver,
    Parameter(u32),
    /// The throwable that left the lambda invocation, and `null` on the normal exit.
    Cause,
    /// The result of invoking the function-typed parameter.
    Invocation,
    /// The result of an earlier call in the SAME list.
    Call(u32),
}

/// One exit from the guarded invocation: the calls it makes and the value it yields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineArm {
    pub calls: Box<[FirInlineCall]>,
    pub value: FirInlineValue,
}

/// One call a checked inline plan makes around its lambda invocation. The declaration is an opaque
/// identity; target-specific owners, descriptors and invocation opcodes remain provider data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirInlineCall {
    pub declaration: ExternalCallableId,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub suspend: bool,
    /// Dispatch receiver, for a call on a member; `None` for a top-level function.
    pub dispatch: Option<FirInlineValue>,
    pub arguments: Box<[FirInlineValue]>,
}

/// A parameter the caller may leave out, with the value the declaration's own default supplies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirInlineDefault {
    pub parameter: u32,
    pub value: FirInlineDefaultValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirInlineBodyPlan {
    /// Invoke one function-typed parameter, optionally entering a region before it and leaving that
    /// region from `finally`.
    ///
    /// Every "set up, call the lambda, tear down" declaration checks to this one shape; they differ
    /// only in which fields are populated. `let`/`run`/`with` have neither prologue nor cleanup,
    /// `apply`/`also` return a parameter instead of the invocation result, `Mutex.withLock` and
    /// `Semaphore.withPermit` enter a suspending region and leave it from `finally`, and
    /// `Closeable.use` hands its cleanup the throwable that left the body.
    InvokeLambda {
        lambda_parameter: u32,
        arguments: Box<[FirInlineValue]>,
        prologue: Box<[FirInlineCall]>,
        /// The normal exit: what runs after the invocation, and the value the body yields.
        normal: FirInlineArm,
        /// The exit taken when a throwable leaves the invocation and the body produces a value from
        /// it rather than rethrowing.
        recover: Option<FirInlineArm>,
        cleanup: Box<[FirInlineCall]>,
        records_cause: bool,
        defaults: Box<[FirInlineDefault]>,
    },
    /// Declaration-scoped iterator expansion for the exact selected inline `forEach` declaration.
    /// All three convention calls were selected by the checker at the call site; lowering only
    /// splices the checked lambda body into the resulting loop.
    ForEach {
        lambda_parameter: u32,
        /// Whether the lambda takes the element's zero-based position before the element itself.
        /// `forEachIndexed` is `forEach` plus that counter, and nothing else about the loop differs.
        indexed: bool,
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
