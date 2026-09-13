//! Provider-normalized contracts for source-independent inline expansion.

use super::{DefaultValue, LibraryMember};

/// A value an inline body hands to one of the calls it makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineBodyValue {
    /// A parameter of the inline declaration, by ordinal. The extension receiver is parameter 0.
    Parameter(usize),
    /// The throwable that left the lambda invocation, and `null` on the normal exit. Only a plan
    /// whose body records one produces this.
    Cause,
}

/// One call an inline body makes around its lambda invocation.
#[derive(Clone, Debug)]
pub struct InlineBodyCall {
    /// The selected declaration, as an opaque provider handle.
    pub member: Box<LibraryMember>,
    /// Dispatch receiver, for a call on a member; `None` for a top-level function.
    pub dispatch: Option<InlineBodyValue>,
    pub arguments: Vec<InlineBodyValue>,
}

/// A parameter the caller may leave out, with the value the declaration's own default supplies.
#[derive(Clone, Debug)]
pub struct InlineBodyDefault {
    pub parameter: usize,
    pub value: DefaultValue,
}

#[derive(Clone, Debug)]
pub enum InlineBodyPlan {
    /// Invoke one function-typed parameter, optionally entering a region before it and leaving that
    /// region from `finally`.
    ///
    /// Every "set up, call the lambda, tear down" declaration decodes to this one shape; they differ
    /// only in which fields are populated. `let`/`run`/`with` have neither prologue nor cleanup,
    /// `apply`/`also` return a parameter instead of the invocation result, `Mutex.withLock` and
    /// `Semaphore.withPermit` enter a suspending region and leave it from `finally`, and
    /// `Closeable.use` hands its cleanup the throwable that left the body.
    InvokeLambda {
        lambda_parameter: usize,
        /// Values passed to the invocation.
        arguments: Vec<InlineBodyValue>,
        /// Calls made before the invocation, in body order.
        prologue: Vec<InlineBodyCall>,
        /// Calls made on BOTH exits from the invocation, in body order. A non-empty cleanup means
        /// the invocation is guarded by a `try`/`finally`.
        cleanup: Vec<InlineBodyCall>,
        /// Whether the body stores the escaping throwable and rethrows it, so that
        /// [`InlineBodyValue::Cause`] has a value to name.
        records_cause: bool,
        /// Parameters the caller may omit. Any other omitted argument means the call site is not
        /// this expansion.
        defaults: Vec<InlineBodyDefault>,
        /// A value returned in place of the invocation result (`apply` returns its receiver).
        result: Option<InlineBodyValue>,
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

#[derive(Clone, Debug)]
pub struct InlineCollectionLocalNames {
    pub outer_receiver: Box<str>,
    pub inner_receiver: Box<str>,
    pub destination: Box<str>,
    pub element: Box<str>,
}

impl InlineBodyPlan {
    /// The unguarded form of [`InlineBodyPlan::InvokeLambda`] — invoke the lambda and yield, with
    /// nothing set up before it and nothing torn down after — as plain parameter ordinals.
    ///
    /// `None` for every other plan, a guarded one included: an expansion that drops the `try` its
    /// declaration compiled would leave the cleanup unrun.
    pub fn plain_invoke_lambda(&self) -> Option<(usize, Vec<usize>, Option<usize>)> {
        let InlineBodyPlan::InvokeLambda {
            lambda_parameter,
            arguments,
            prologue,
            cleanup,
            result,
            ..
        } = self
        else {
            return None;
        };
        if !prologue.is_empty() || !cleanup.is_empty() {
            return None;
        }
        let parameter = |value: &InlineBodyValue| match value {
            InlineBodyValue::Parameter(parameter) => Some(*parameter),
            InlineBodyValue::Cause => None,
        };
        Some((
            *lambda_parameter,
            arguments
                .iter()
                .map(parameter)
                .collect::<Option<Vec<_>>>()?,
            match result {
                None => None,
                Some(result) => Some(parameter(result)?),
            },
        ))
    }
}
