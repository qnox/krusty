//! The checked shape of a DELEGATED property: the conventions its accessors call, and the plan
//! that names them.
//!
//! A delegated property is resolved once and lowered many times, so what the checker decided has to
//! travel as data rather than be re-derived. These types are that data: which callable each
//! convention selected, the applied and declared shape of every slot a value crosses into, the
//! receiver each one takes, and the classifier the `KProperty` argument was resolved to. Lowering
//! reads them and asks resolution nothing.

use crate::types::TypeName;

use super::{FirCallTarget, ResolvedTy};

/// One checker-selected delegated-property convention. `extension` records receiver placement;
/// the target itself is already a stable module/provider identity with its final semantic types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirDelegateCall {
    pub target: FirCallTarget,
    /// The value parameters the checker SELECTED, in order, with the extension receiver already
    /// taken out — the applied shape (`setValue(…, newValue: T)` at `T := Long` is `Long` here).
    pub parameters: Box<[ResolvedTy]>,
    /// The same slots as the DECLARATION spells them, before this call's substitution: a type
    /// parameter stays a type parameter, so `setValue(…, newValue: T)` reads `T` here where
    /// [`Self::parameters`] reads whatever `T` was selected as.
    ///
    /// Deliberately not the erased ABI list. What a `T` slot costs a value — a box, a cast, nothing
    /// — is a target's answer and differs between targets; common lowering states only that the two
    /// SEMANTIC types differ, and a backend reads its own physical slots off that. Always the same
    /// length as [`Self::parameters`], which is checked where the plan is published.
    pub declared_parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub extension: bool,
    pub dispatch_receiver: Option<FirDelegateDispatchReceiver>,
    /// The CHECKED type of the value that reaches the convention's receiver slot — the stored
    /// delegate for `getValue`/`setValue`, the delegate EXPRESSION for `provideDelegate`. Paired
    /// with [`Self::declared_receiver`] it is the whole adaptation fact: lowering compares the two
    /// rather than asking a caller to supply what it happens to know, which is how a
    /// `provideDelegate` receiver reached an erased `Object` slot unboxed.
    pub receiver: ResolvedTy,
    /// The receiver type the selected callable DECLARES, when it takes one as an extension. `None`
    /// for a member convention, whose receiver is the dispatch object rather than an argument.
    pub declared_receiver: Option<ResolvedTy>,
}

/// Exact implicit dispatch receiver selected for a member-extension delegate convention. The
/// delegate storage remains the extension receiver; this coordinate identifies the independent
/// receiver that owns the selected convention declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirDelegateDispatchReceiver {
    Scoped {
        ty: ResolvedTy,
        current: bool,
        depth: u32,
    },
    ContextBinding {
        ty: ResolvedTy,
        name: Box<str>,
        shadow_depth: u32,
    },
    Singleton {
        ty: ResolvedTy,
        classifier: TypeName,
    },
}

/// Declaration-level semantics attached only to a delegated-property body unit. The ordinary body
/// arena still owns the delegate initializer expression; this compact plan is enough for common
/// lowering to synthesize storage and accessor bodies without retaining syntax or resolver state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirPropertyDelegatePlan {
    pub storage_type: ResolvedTy,
    /// The type of the `KProperty` value the conventions receive, as RESOLUTION selected it — the
    /// same classifier it used to decide applicability. Lowering materializes a static holding this
    /// and passes it to every convention; publishing it here keeps one answer to what that
    /// classifier is, instead of the checker deciding applicability against one spelling and
    /// lowering inventing another for the value it builds.
    pub property_reference_type: ResolvedTy,
    pub provide_delegate: Option<FirDelegateCall>,
    pub get_value: FirDelegateCall,
    pub set_value: Option<FirDelegateCall>,
}
