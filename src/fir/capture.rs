//! Checked value sources for a nested callable's captures.
//!
//! A lambda or local function takes what it captures as leading parameters, and this is where each
//! one is read from in the body that supplies it. Keeping the coordinates here rather than in the
//! body facade means one place decides what a capture can name, and lowering consumes exactly that.

use super::header::{DeclarationId, LocalValueId, OriginId};
use super::signature::ResolvedTy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirCapture {
    pub origin: OriginId,
    pub enclosing_depth: u32,
    pub source: FirCaptureSource,
    pub ty: ResolvedTy,
    pub shared_cell: bool,
}

/// Where a nested callable's capture is read from in the body that supplies it.
///
/// Almost every capture names a value slot of the enclosing body. The exception is a capture
/// reached while a CONSTRUCTOR PREFIX is in scope — a superclass or sibling-constructor argument,
/// or a constructor default. There the instance does not exist yet, so the enclosing class's
/// capture is only reachable through the constructor's synthetic prefix parameter; reading it off
/// the storage would load it from a `this` the JVM verifier will not let you touch, and that a
/// target without a verifier answers with a zero.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FirCaptureSource {
    /// A value slot of the enclosing body.
    Value(LocalValueId),
    /// The enclosing constructor's synthetic prefix parameter for `owner`'s capture field.
    ConstructorPrefix { owner: DeclarationId, field: u32 },
}

/// Where a read of a constructor's synthetic prefix parameter finds its value.
///
/// The two are different lookups, and the checker is the only thing that knows which one a given
/// read is: in the constructor body the parameter is in scope and is read directly, while in a
/// callable nested in the prefix the value arrived as one more capture and is read from the slot
/// registered at the depth recorded here. Carrying the answer means lowering consumes the checked
/// binding instead of trying one lookup and reinterpreting the node when it misses — which would
/// bind whatever same-owner/field entry happened to be found first.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FirConstructorCaptureSite {
    /// The constructor body itself: the synthetic prefix parameter is in scope.
    Parameter,
    /// A callable nested in the constructor prefix. The value is one of THIS body's captures,
    /// keyed — like every other capture — by the depth of the body that supplies it.
    Captured { enclosing_depth: u32 },
}

impl FirCaptureSource {
    /// The enclosing value slot, when this capture names one.
    pub fn value(self) -> Option<LocalValueId> {
        match self {
            Self::Value(value) => Some(value),
            Self::ConstructorPrefix { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirImplicitReceiverCapture {
    pub origin: OriginId,
    pub enclosing_depth: u32,
    pub current: bool,
    pub depth: u32,
    /// Exact enclosing-instance edges selected in the source body that supplies this capture.
    /// Empty means an ordinary lexical receiver slot. A non-empty path is interpreted only at the
    /// capture site; nested forwarding retains it unchanged and never repeats classifier lookup.
    pub path: Box<[DeclarationId]>,
    pub ty: ResolvedTy,
}
