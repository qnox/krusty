//! Inline scope numbers (`-Xuse-inline-scopes-numbers`) for inline bodies expanded in IR.
//!
//! The method's numbering is shared with the bytecode inliner, which numbers each body as it
//! places it, so an IR expansion takes its numbers as it closes: the callee's scopes in the order
//! they start, then each lambda it placed, in the order of its `invoke`s, after the scopes that
//! lambda's own body numbered. A placed lambda's body numbers its scopes from a generator of its
//! own while it is emitted, as kotlinc compiles it as a method of its own. The table spells the
//! names once it is complete (see `classfile::inline_scope_frames`).

use super::*;
use crate::ir::IrDebugLocalProvenance;
use crate::jvm::classfile::inline_scope_frames::{InlineScopeFrame, InlineScopeFrameKind};

impl Emitter<'_> {
    /// Register the frame the marker `declaration` opened in `code`, inside `parent`.
    pub(super) fn open_inline_scope_frame(
        &mut self,
        declaration: crate::ir::ExprId,
        frame: u64,
        parent: Option<u64>,
        code: &mut CodeBuilder,
    ) {
        if self.inline_scopes.is_none() {
            return;
        }
        let unit = self.inline_scope_units.last().map(|&(unit, _)| unit);
        let kind = match self.ir.debug_local_provenance(declaration) {
            Some(IrDebugLocalProvenance::FunctionFrameMarker) => InlineScopeFrameKind::Function,
            Some(IrDebugLocalProvenance::LambdaFrameMarker { depth, .. }) => {
                let unit_depth = unit
                    .and_then(|unit| {
                        code.inline_scope_frames()
                            .iter()
                            .find(|frame| frame.id == unit)
                    })
                    .and_then(|frame| match frame.kind {
                        InlineScopeFrameKind::Lambda { depth, .. } => Some(depth),
                        InlineScopeFrameKind::Function => None,
                    })
                    .unwrap_or(0);
                InlineScopeFrameKind::Lambda {
                    depth,
                    placed: depth == unit_depth,
                }
            }
            _ => return,
        };
        let placed = matches!(kind, InlineScopeFrameKind::Lambda { placed: true, .. });
        // The marker of an `@InlineOnly` callee's lambda can carry the synthetic inline line;
        // the call's line is the one before it.
        let marks = code.line_marks();
        let synthetic = self.ir.inline_synthetic_lines.contains(&declaration);
        let call_site_line = marks
            .len()
            .checked_sub(if synthetic { 2 } else { 1 })
            .map(|at| marks[at].1);
        code.register_inline_scope_frame(InlineScopeFrame {
            id: frame,
            kind,
            parent,
            unit,
            call_site_line,
            scope: 0,
            count: 0,
        });
        if placed {
            let caller = self.inline_scopes.replace(Default::default());
            self.inline_scope_units.push((frame, caller));
        }
    }

    /// Number what closing `frame` completes: a placed lambda's body, and the inlining a frame
    /// with no enclosing inlining in its body ends.
    pub(super) fn close_inline_scope_frame(&mut self, frame: u64, code: &mut CodeBuilder) {
        let Some(record) = code
            .inline_scope_frames()
            .iter()
            .find(|candidate| candidate.id == frame)
            .cloned()
        else {
            return;
        };
        let mut count = 0;
        if record.is_placed()
            && self.inline_scope_units.last().map(|&(unit, _)| unit) == Some(frame)
        {
            let (_, caller) = self
                .inline_scope_units
                .pop()
                .expect("the placed lambda's body is the innermost");
            count = 1 + self.inline_scopes.map_or(0, |scopes| scopes.inlined);
            self.inline_scopes = caller;
            if let Some(placed) = code.inline_scope_frame_mut(frame) {
                placed.count = count;
            }
        }
        let frames = code.inline_scope_frames();
        if inside_inlining(frames, &record) {
            return;
        }
        let Some(mut scopes) = self.inline_scopes else {
            return;
        };
        match record.kind {
            // A lambda an `@InlineOnly` callee places: the callee has no scopes of its own.
            InlineScopeFrameKind::Lambda { placed: true, .. } => {
                scopes.inlined += 1;
                let scope = scopes.inlined;
                scopes.inlined += count - 1;
                if let Some(placed) = code.inline_scope_frame_mut(frame) {
                    placed.scope = scope;
                }
            }
            InlineScopeFrameKind::Lambda { placed: false, .. } => {}
            InlineScopeFrameKind::Function => {
                let by_start = |ids: Vec<u64>| -> Vec<u64> {
                    let mut ids: Vec<(Option<u16>, usize, u64)> = ids
                        .into_iter()
                        .enumerate()
                        .map(|(order, id)| (code.inline_frame_start(id), order, id))
                        .collect();
                    ids.sort_unstable();
                    ids.into_iter().map(|(_, _, id)| id).collect()
                };
                let callee = by_start(
                    frames
                        .iter()
                        .filter(|candidate| {
                            !candidate.is_placed() && placed_by(frames, candidate, frame, true)
                        })
                        .map(|candidate| candidate.id)
                        .collect(),
                );
                let lambdas = by_start(
                    frames
                        .iter()
                        .filter(|candidate| {
                            candidate.is_placed() && placed_by(frames, candidate, frame, false)
                        })
                        .map(|candidate| candidate.id)
                        .collect(),
                );
                for id in callee {
                    scopes.inlined += 1;
                    if let Some(scope) = code.inline_scope_frame_mut(id) {
                        scope.scope = scopes.inlined;
                    }
                }
                for id in lambdas {
                    let Some(lambda) = code.inline_scope_frame_mut(id) else {
                        continue;
                    };
                    lambda.scope = scopes.inlined + 1;
                    scopes.inlined += lambda.count;
                }
            }
        }
        self.inline_scopes = Some(scopes);
    }
}

fn frame_by_id(frames: &[InlineScopeFrame], id: u64) -> Option<&InlineScopeFrame> {
    frames.iter().find(|frame| frame.id == id)
}

/// Whether `frame` is part of an inlining that encloses it in its own body.
fn inside_inlining(frames: &[InlineScopeFrame], frame: &InlineScopeFrame) -> bool {
    let mut next = frame.parent;
    while let Some(id) = next.filter(|&id| Some(id) != frame.unit) {
        let Some(ancestor) = frame_by_id(frames, id) else {
            return false;
        };
        if ancestor.kind == InlineScopeFrameKind::Function {
            return true;
        }
        next = ancestor.parent;
    }
    false
}

/// Whether `frame` belongs to the inlining `root` opens: `root` itself or a scope of its callee
/// (`itself`), or a lambda `root` placed, without another placed lambda between them.
fn placed_by(
    frames: &[InlineScopeFrame],
    frame: &InlineScopeFrame,
    root: u64,
    itself: bool,
) -> bool {
    if frame.id == root {
        return itself;
    }
    let mut next = frame.parent;
    while let Some(id) = next {
        if id == root {
            return true;
        }
        let Some(ancestor) = frame_by_id(frames, id) else {
            return false;
        };
        if ancestor.is_placed() {
            return false;
        }
        next = ancestor.parent;
    }
    false
}
