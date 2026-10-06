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

/// Typed exceptional arm of an exact declaration-owned inline body. Both dependencies are
/// metadata-normalized declarations; a backend remains solely responsible for how the result
/// classifier and its constructor are represented physically.
#[derive(Clone, Debug)]
pub struct InlineBodyRecovery {
    /// Exact classifier caught by the declaration's exception table.
    pub caught: crate::types::Ty,
    /// Semantic constructor wrapping both the normal lambda value and the failure payload.
    pub constructor: Box<LibraryMember>,
    /// Exact declaration that converts the caught value into the constructor's failure payload.
    pub failure: Box<InlineBodyCall>,
}

/// How an exact declaration advances the index supplied to its iteration lambda.
#[derive(Clone, Debug)]
pub enum InlineIterationIndex {
    /// The receiver's bounded traversal cannot overflow an Int index.
    Unchecked,
    /// The declaration checks the old post-increment value and calls this normalized target when it
    /// is negative.
    Checked { overflow: Box<InlineBodyCall> },
}

/// Provider-normalized traversal performed by an exact declaration-owned iteration body.
/// Member calls retain their stable declaration records; arrays use target-neutral operations
/// because their length/load instructions do not name callable declarations.
#[derive(Clone, Debug)]
pub enum InlineIterationTraversal {
    Iterator {
        /// Zero-argument calls applied left-to-right, beginning with the inline receiver and ending
        /// in the iterator consumed by `has_next`/`next`. Declarations whose receiver must first
        /// expose an intermediate traversal view therefore carry more than one exact step.
        prepare: Vec<LibraryMember>,
        has_next: Box<LibraryMember>,
        next: Box<LibraryMember>,
    },
    Array,
    Counted {
        size: Box<LibraryMember>,
        get: Box<LibraryMember>,
    },
}

#[derive(Clone, Debug)]
pub enum InlineCollectionCapacity {
    Member(Box<LibraryMember>),
    Extension {
        callable: Box<LibraryCallable>,
        default: i32,
    },
}

#[derive(Clone, Debug)]
pub enum InlineCollectionAppend {
    Member(Box<LibraryMember>),
    Extension(Box<LibraryCallable>),
}

/// The inline declaration's own frame facts: the part of kotlinc's inline debug surface the
/// expansion must reproduce. Providers normalize these from the exact declaration's compiled
/// body; a backend formats them for its target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineBodyFrame {
    /// Simple source name of the declaration (`apply`, `forEach`). It spells the frame's
    /// `$i$f$<name>` function marker and each lambda argument's `$i$a$-<name>-…` marker.
    pub callee: Box<str>,
    /// The declaration is `@InlineOnly`: its body carries no function marker, no named frame
    /// locals, and no mappable lines of its own, which is exactly why its decode template has no
    /// `iconst_0; istore` marker store. The expansion takes the call site's lines; only the
    /// lambda argument marker takes the target's synthetic inline line.
    pub inline_only: bool,
    /// The declaration body's own debug surface. `None` for an inline-only declaration, whose
    /// lines and locals are dropped at the call site.
    pub source: Option<InlineBodySource>,
}

/// Debug-source identity of a non-inline-only declaration body, resolved through the body's own
/// debug mapping at the provider boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineBodySource {
    /// Source file the body's lines belong to (`_Collections.kt`) and the path they map under
    /// (the class its code was read from, or the class its own debug mapping retargets).
    pub file: Box<str>,
    pub path: Box<str>,
    /// Resolved source line of the body's lambda invocation — the `for` line of an iteration
    /// body, covering its traversal, element store, and invoke.
    pub invoke_line: u16,
    /// Resolved source line the body's frame closes on (the declaration's closing line).
    pub close_line: u16,
    /// Source name of the body's iteration element local (`element`), when it declares one. The
    /// extension receiver's local is named from `callee` by the target backend, not stored here.
    pub element: Option<Box<str>>,
}

/// A declaration-defined inline body whose source-independent control-flow shape must be expanded
/// before backend coroutine lowering. Providers decode this from the exact selected declaration's
/// compiled inline body; source spelling never participates.
#[derive(Clone, Debug)]
pub enum InlineBodyPlan {
    /// Invoke one function-typed parameter, optionally entering a region before it and leaving the
    /// region from `finally`.
    InvokeLambda {
        frame: InlineBodyFrame,
        lambda_parameter: usize,
        arguments: Vec<InlineBodyValue>,
        prologue: Vec<InlineBodyCall>,
        cleanup: Vec<InlineBodyCall>,
        /// Semantic type of the throwable local recorded by the exact catch template. `None` when
        /// the lambda is not wrapped in such a catch.
        cause: Option<crate::types::Ty>,
        /// Typed value-producing catch arm. This is distinct from `cause`, which records and
        /// rethrows solely to implement a `finally` cleanup contract.
        recovery: Option<Box<InlineBodyRecovery>>,
        defaults: Vec<InlineBodyDefault>,
        /// A declaration parameter returned instead of the invocation result (`apply`/`also`).
        result: Option<InlineBodyValue>,
    },
    /// Iterate the extension receiver and invoke one function-typed parameter with the exact
    /// declaration-owned argument order. An indexed declaration supplies its normalized overflow
    /// call when the source loop is not statically bounded.
    Iteration {
        frame: InlineBodyFrame,
        lambda_parameter: usize,
        index: Option<InlineIterationIndex>,
        traversal: InlineIterationTraversal,
    },
    /// Iterate the extension receiver, invoke one lambda for each element, and append its result to
    /// a fresh collection. The provider owns the exact factory and append declarations; consumers
    /// see only their stable identities after selection. The append operation itself distinguishes
    /// a member append from an extension append-all; there is no separate name-derived mode.
    CollectionTransform {
        lambda_parameter: usize,
        /// Exact traversal declarations read from the selected declaration's compiled body. The
        /// resolver specializes their metadata signatures to the selected receiver before FIR
        /// publication; callers' scopes and same-spelled operators never participate.
        traversal: InlineIterationTraversal,
        /// Source-local names retained from the selected declaration's inline body. These are
        /// provider facts, not target formatting; common lowering carries them as debug provenance.
        local_names: InlineCollectionLocalNames,
        factory: Box<LibraryMember>,
        capacity: Option<InlineCollectionCapacity>,
        append: InlineCollectionAppend,
    },
}

#[derive(Clone, Debug)]
pub struct InlineCollectionLocalNames {
    pub outer_receiver: Box<str>,
    pub inner_receiver: Box<str>,
    pub destination: Box<str>,
    pub element: Box<str>,
}
