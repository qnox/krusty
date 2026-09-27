//! What a local class or anonymous object captures at its construction site: the checker's
//! contract with checked FIR, which turns each capture into a constructor parameter and field.

use super::scope::ContextReceiverKind;
use super::{Checker, CheckerScope};
use crate::diag::Span;
use crate::fir::{CapturedContextKind, FirCapturedReceiver};
use crate::types::Ty;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnonymousObjectCapture {
    pub name: String,
    pub ty: Ty,
    /// The enclosing mutable local is represented by one shared cell rather than copied by value.
    /// This is a semantic capture decision; the backend chooses the cell representation.
    pub shared_cell: bool,
    /// Physical value captured by the generated class. A delegated property's semantic type remains
    /// `ty`, while its immutable delegate object is the constructor/field payload.
    pub storage_ty: Option<Ty>,
    /// Semantic source of the captured value. Keep this separate from `name`: `this$0` is one JVM
    /// field spelling, not a reliable front-end discriminator. A backend or future target may choose
    /// a different physical name while the enclosing-instance meaning remains unchanged.
    pub source: AnonymousObjectCaptureSource,
    /// Source-level receiver label required while a retained inline/local classifier body is
    /// checked in isolation. This is present only for an implicit receiver capture; checked FIR
    /// consumes the semantic receiver coordinate and does not retain the spelling.
    pub receiver_label: Option<Box<str>>,
    /// What a captured receiver was in source, which names the capture's field; `None` for a
    /// captured value.
    pub receiver: Option<FirCapturedReceiver>,
    /// Number of distinct same-named lexical bindings nearer than the selected source at this
    /// construction site. This is a bounded-checker coordinate, not a source location; checked FIR
    /// consumes it while the active lexical scopes still exist.
    pub(crate) lexical_shadow_depth: u32,
    /// Semantic closure field forwarded by this capture. Direct captures leave this absent and
    /// establish their own identity when checked; transitive captures preserve the upstream field.
    pub(crate) capture_dependency: Option<crate::fir::ClassCaptureIdentity>,
}

impl AnonymousObjectCapture {
    pub fn stored_ty(&self) -> Ty {
        self.storage_ty.unwrap_or(self.ty)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnonymousObjectCaptureSource {
    LexicalValue,
    /// A field of the construction site's current classifier, selected by the checker. The capture
    /// constructor reads this exact ordinal; neither FIR construction nor lowering looks the name up.
    ClassStorage {
        field: u32,
    },
    /// Exact class-receiver rung selected at the construction site.
    EnclosingInstance {
        current: bool,
        depth: u32,
    },
    /// A receiver introduced by an enclosing extension/receiver-function/context rung. The
    /// coordinate is relative to the construction body's checked receiver tower; it is semantic
    /// identity and must not be reconstructed from the capture field spelling.
    ImplicitReceiver {
        current: bool,
        depth: u32,
    },
}

impl Checker<'_> {
    /// What the implicit receiver `identity` was in source: the enclosing class instance, a context
    /// parameter of its kind, the extension receiver of the named callable declared at `extension`,
    /// or a receiver lambda's.
    pub(super) fn captured_receiver(
        &self,
        scope: &CheckerScope<'_>,
        identity: (usize, usize),
        extension: Option<Span>,
        class_receiver: bool,
    ) -> FirCapturedReceiver {
        if class_receiver {
            return FirCapturedReceiver::Enclosing;
        }
        if let Some(context) = scope.implicit_receiver_context(identity) {
            let kind = match context.kind {
                ContextReceiverKind::Anonymous => CapturedContextKind::Anonymous,
                ContextReceiverKind::FunctionType => CapturedContextKind::FunctionType,
                ContextReceiverKind::LegacyReceiver => CapturedContextKind::LegacyReceiver,
                ContextReceiverKind::Named => unreachable!("a named context value is no receiver"),
            };
            let types = context.types.into_iter().map(|ty| {
                crate::fir::ResolvedTy::new(ty).expect("a context parameter's type is published")
            });
            return FirCapturedReceiver::Context {
                kind,
                types: types.collect(),
                index: u32::try_from(context.index).expect("too many context parameters"),
            };
        }
        let Some(declaration) = extension else {
            let label = scope.implicit_receiver_lambda_label(identity);
            return FirCapturedReceiver::Lambda(label.map(String::into_boxed_str));
        };
        let (index, _) = self
            .extension_receiver_labels
            .iter()
            .rev()
            .find(|(_, candidate)| *candidate == declaration)
            .expect("an extension callable's receiver is labeled while its body is checked");
        let (label, _, _) = &self.this_labels[*index];
        FirCapturedReceiver::Callable(label.clone().into_boxed_str())
    }
}
