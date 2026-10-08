//! Where each FIR node came from: a source span, or the node a synthetic one was made for.

use super::super::header::{next_id, OriginId, SourceFileId};
use crate::diag::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntheticOriginKind {
    ImplicitReceiver,
    ImplicitConversion,
    DefaultArgument,
    VarargArray,
    StringTemplateLiteral,
    MissingElseUnit,
    GeneratedAccessor,
    GeneratedControlFlow,
    /// A backend lowering's in-place realization of a call of an inline function that kotlinc
    /// inlines, such as the `toInt()` an unsigned counted loop converts a bound with: the call
    /// leaves no dispatch of its own, only the end of an inlined call.
    InlinedCall,
    /// An implicit context argument a call passes. kotlinc builds it without source offsets, so it
    /// carries no line of its own: the call's line is marked at its dispatch.
    ContextArgument,
    /// An operand a compiler plugin's expression plan composes from a frontend-selected
    /// declaration rather than from source syntax, such as the companion accessor call that
    /// supplies kotlinx's `serializer<T>()`. It belongs to the planned expression's line.
    PluginOperand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    Source {
        file: SourceFileId,
        span: Span,
    },
    Synthetic {
        cause: OriginId,
        kind: SyntheticOriginKind,
    },
}

#[derive(Debug, Default)]
pub struct OriginStore {
    origins: Vec<Origin>,
}

impl OriginStore {
    pub fn source(&mut self, file: SourceFileId, span: Span) -> OriginId {
        self.push(Origin::Source { file, span })
    }

    pub fn synthetic(&mut self, cause: OriginId, kind: SyntheticOriginKind) -> OriginId {
        assert!(
            self.get(cause).is_some(),
            "a synthetic FIR origin must reference an existing cause"
        );
        self.push(Origin::Synthetic { cause, kind })
    }

    fn push(&mut self, origin: Origin) -> OriginId {
        let id = OriginId::from_raw(next_id(self.origins.len(), "origins"));
        self.origins.push(origin);
        id
    }

    pub fn get(&self, id: OriginId) -> Option<Origin> {
        self.origins.get(id.raw() as usize).copied()
    }

    /// The source occurrence that ultimately caused `id`.
    ///
    /// A synthetic origin can only name an origin already stored by [`Self::synthetic`], so this
    /// chain is finite by construction and needs no arbitrary traversal limit.
    pub fn source_span(&self, mut id: OriginId) -> Option<(SourceFileId, Span)> {
        loop {
            match self.get(id)? {
                Origin::Source { file, span } => return Some((file, span)),
                Origin::Synthetic { cause, .. } => id = cause,
            }
        }
    }

    pub fn len(&self) -> usize {
        self.origins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
    }

    pub(in crate::fir) fn storage_payload_bytes(&self) -> usize {
        self.origins.len() * std::mem::size_of::<Origin>()
    }
}
