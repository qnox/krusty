//! Provider-normalized contracts for source-independent inline expansion.

use super::{DefaultValue, LibraryCallable, LibraryMember};

/// A value an inline body hands to one of the calls it makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineBodyValue {
    /// A parameter of the inline declaration, by ordinal. The extension receiver is parameter 0.
    Parameter(usize),
    /// The throwable that left the lambda invocation, and `null` on the normal exit.
    Cause,
}

/// The source-level receiver role of one semantic call in an inline body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineBodyCallReceiver {
    Dispatch(InlineBodyValue),
    Extension(InlineBodyValue),
}

/// One semantic call the declaration makes around its lambda invocation.
#[derive(Clone, Debug)]
pub struct InlineBodyCall {
    /// Metadata-normalized declaration. JVM data identifies only its physical realization. Its
    /// stable identity is assigned to a per-classpath clone after plan-cache lookup.
    pub callable: Box<LibraryCallable>,
    /// Dispatch or extension receiver; `None` only for a receiver-less top-level call.
    pub receiver: Option<InlineBodyCallReceiver>,
    pub arguments: Vec<InlineBodyValue>,
}

/// A parameter the caller may omit, paired with the declaration's own decoded default.
#[derive(Clone, Debug)]
pub struct InlineBodyDefault {
    pub parameter: usize,
    pub value: DefaultValue,
}

/// A declaration-defined inline body whose source-independent control-flow shape must be expanded
/// before backend coroutine lowering. Providers decode this from the exact selected declaration's
/// compiled inline body; source spelling never participates.
#[derive(Clone, Debug)]
pub enum InlineBodyPlan {
    /// Invoke one function-typed parameter, optionally entering a region before it and leaving the
    /// region from `finally`.
    InvokeLambda {
        lambda_parameter: usize,
        arguments: Vec<InlineBodyValue>,
        prologue: Vec<InlineBodyCall>,
        cleanup: Vec<InlineBodyCall>,
        records_cause: bool,
        defaults: Vec<InlineBodyDefault>,
        /// A declaration parameter returned instead of the invocation result (`apply`/`also`).
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
    /// Return the unguarded lambda-only subset consumed by the parser-era migration path.
    /// Checked FIR consumes every other plan directly and unconditionally.
    pub fn plain_invoke_lambda(&self) -> Option<(usize, Vec<usize>, Option<usize>)> {
        let Self::InvokeLambda {
            lambda_parameter,
            arguments,
            prologue,
            cleanup,
            records_cause,
            defaults,
            result,
        } = self
        else {
            return None;
        };
        if !prologue.is_empty() || !cleanup.is_empty() || *records_cause || !defaults.is_empty() {
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
