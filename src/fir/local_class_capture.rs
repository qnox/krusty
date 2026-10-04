//! Checked value sources for local-class and anonymous-object constructor captures.

use super::header::{DeclarationId, FirExprId, LocalValueId, OriginId};
use super::signature::ResolvedTy;
use super::ClassCaptureIdentity;
use crate::types::CapturedContextKind;

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

/// What a captured receiver was in source. A target names the capture's field and constructor
/// parameter after it; kotlinc's `LocalDeclarationsLowering` names each kind differently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirCapturedReceiver {
    /// The enclosing class instance.
    Enclosing,
    /// The extension receiver of the named callable with this source name, and whether that
    /// callable is a local function or the declaration the capture is written in.
    Callable {
        label: Box<str>,
        owner: CapturedCallableOwner,
    },
    /// A lambda's or anonymous function's receiver, with the lambda's label when it has one.
    Lambda(Option<Box<str>>),
    /// A context parameter that is an implicit receiver: its kind, the declared types of its
    /// rung's context parameters of that kind, in order, and its own position among them.
    Context {
        kind: CapturedContextKind,
        types: Box<[ResolvedTy]>,
        index: u32,
    },
}

/// Which callable declares a captured extension receiver: a local function, or the member or
/// top-level declaration (function or property accessor) whose body the capture is written in.
/// A target may realize the two differently: a value class's member keeps its extension receiver
/// as an ordinary parameter of the static it lowers to, while a local function keeps a receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturedCallableOwner {
    Declaration,
    LocalFunction,
}

impl FirCapturedReceiver {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        match self {
            Self::Enclosing | Self::Lambda(None) => 0,
            Self::Callable { label, .. } | Self::Lambda(Some(label)) => label.len(),
            Self::Context { types, .. } => types.len() * std::mem::size_of::<ResolvedTy>(),
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
    /// `Some` exactly when the captured value is a receiver.
    pub receiver: Option<FirCapturedReceiver>,
}

impl FirLocalClassCapture {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.name.len()
            + self.source.storage_payload_bytes()
            + self
                .receiver
                .as_ref()
                .map_or(0, FirCapturedReceiver::storage_payload_bytes)
    }
}
