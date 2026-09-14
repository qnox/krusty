//! Provider-normalized contracts for source-independent inline expansion.

use super::{DefaultValue, LibraryMember};

/// A declaration-defined inline body whose source-independent control-flow shape must be expanded
/// before backend coroutine lowering. Providers decode this from the exact selected declaration's
/// compiled inline body; source spelling never participates.
#[derive(Clone, Debug)]
pub enum InlineBodyPlan {
    /// Invoke one function-typed parameter with values loaded from other callable parameters and
    /// return the invocation result.
    InvokeLambda {
        lambda_parameter: usize,
        argument_parameters: Vec<usize>,
        /// A callable parameter returned after the invocation (`apply` returns its receiver). `None`
        /// means the invocation result itself is returned (`let`, `run`, `with`).
        return_parameter: Option<usize>,
    },
    /// Invoke a suspending member on the extension receiver, invoke one lambda parameter, and invoke
    /// a cleanup member on normal and exceptional exits.
    ///
    /// `state` is the optional argument both members take besides the receiver: `Mutex.withLock`
    /// passes its `owner` to `lock`/`unlock`, while `Semaphore.withPermit` passes nothing to
    /// `acquire`/`release`. `None` means both are called with the receiver alone.
    SuspendBeforeLambdaFinally {
        lambda_parameter: usize,
        state: Option<InlineBodyState>,
        enter: Box<LibraryMember>,
        cleanup: Box<LibraryMember>,
    },
    /// Iterate the extension receiver, invoke one lambda for each element, and append its result to
    /// a fresh collection. The provider owns the exact factory and append declarations; consumers
    /// see only their stable identities after selection. `flatten` chooses one-element append versus
    /// append-all, matching the selected declaration's compiled inline body.
    CollectionTransform {
        lambda_parameter: usize,
        flatten: bool,
        /// Source-local names retained from the selected declaration's inline body. These are
        /// provider facts, not target formatting; common lowering carries them as debug provenance.
        local_names: InlineCollectionLocalNames,
        factory: Box<LibraryMember>,
        append: Box<LibraryMember>,
    },
}

/// The one argument a [`InlineBodyPlan::SuspendBeforeLambdaFinally`] body threads through its enter
/// and cleanup members, and the value its own default supplies when the caller omits it.
#[derive(Clone, Debug)]
pub struct InlineBodyState {
    pub parameter: usize,
    pub default: DefaultValue,
}

#[derive(Clone, Debug)]
pub struct InlineCollectionLocalNames {
    pub outer_receiver: Box<str>,
    pub inner_receiver: Box<str>,
    pub destination: Box<str>,
    pub element: Box<str>,
}
