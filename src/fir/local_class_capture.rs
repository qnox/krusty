//! Checked value sources for local-class and anonymous-object constructor captures.

use super::header::{DeclarationId, FirExprId, LocalValueId, OriginId};
use super::signature::ResolvedTy;
use super::ClassCaptureIdentity;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirLocalClassCaptureSource {
    Value(LocalValueId),
    Captured {
        enclosing_depth: u32,
        source: LocalValueId,
    },
    ClassStorage {
        owner: DeclarationId,
        enclosing_depth: u32,
        field: u32,
    },
    /// The capture is read from the current constructor's synthetic prefix parameter rather than
    /// from class storage. Superclass and sibling-constructor arguments run before the instance
    /// exists, so a value forwarded into a nested callable cannot be loaded from `this`.
    ConstructorCapture {
        owner: DeclarationId,
        field: u32,
        /// Where the value is: the prefix parameter itself, or this body's capture of it.
        site: super::FirConstructorCaptureSite,
    },
    CapturedClassStorage {
        owner: DeclarationId,
        receiver: FirExprId,
        path: Box<[DeclarationId]>,
        field: u32,
    },
    DispatchReceiver,
    /// Exact `inner`-classifier edges from the construction body's dispatch receiver to the
    /// enclosing instance being captured. A semantic receiver depth is not a value-slot address;
    /// publishing the declaration path here keeps common lowering mechanical.
    EnclosingReceiver {
        path: Box<[DeclarationId]>,
    },
    /// A receiver owned by an enclosing callable frame and explicitly captured by the current
    /// local callable. The coordinate is the same checked capture identity carried by the owning
    /// body; lowering reads that exact lifted parameter slot.
    CapturedImplicitReceiver {
        enclosing_depth: u32,
        current: bool,
        depth: u32,
        path: Box<[DeclarationId]>,
    },
    ImplicitReceiver {
        current: bool,
        depth: u32,
    },
}

impl FirLocalClassCaptureSource {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::CapturedClassStorage { path, .. }
            | Self::EnclosingReceiver { path }
            | Self::CapturedImplicitReceiver { path, .. } => {
                path.len() * std::mem::size_of::<DeclarationId>()
            }
            Self::Value(_)
            | Self::Captured { .. }
            | Self::ClassStorage { .. }
            | Self::ConstructorCapture { .. }
            | Self::DispatchReceiver
            | Self::ImplicitReceiver { .. } => 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirLocalClassCapture {
    pub origin: OriginId,
    pub name: Box<str>,
    pub ty: ResolvedTy,
    pub shared_cell: bool,
    /// Stable semantic identity of the closure field this capture forwards. A direct capture owns
    /// its current classifier/field coordinate; a transitive capture retains the upstream one.
    /// Common lowering uses this edge instead of joining constructor prefixes by field spelling.
    pub(crate) capture_identity: Option<ClassCaptureIdentity>,
    pub source: FirLocalClassCaptureSource,
}
