//! The order a lifted local function takes its captured values in, as kotlinc's `ClosureAnnotator`
//! builds it.
//!
//! The closure first sees the values the function's own defaults and body read, in the order they
//! read them. The closures of the local functions the body calls or references, of its lambdas and
//! of the anonymous objects it creates come after all of those, each in its own order, as they are
//! met. A local function the body only declares adds nothing, and neither does a call to the
//! function itself.

use std::collections::HashMap;

use crate::fir::{
    BodyLocalCallableDeclarationId, FirBody, FirCapture, FirCaptureSource, FirExprId, FirExprKind,
    FirLocalCallableRef, FirLocalClassCaptureSource, FirStatementId, FirStatementKind,
};

/// The `(enclosing_depth, source)` keys of `function`'s captures in first-use order. `checked`
/// holds the ordered captures of every local function declared outside `function`'s body.
pub(super) fn first_use_captures(
    function: &FirBody,
    declaration: BodyLocalCallableDeclarationId,
    checked: &HashMap<BodyLocalCallableDeclarationId, Box<[FirCapture]>>,
) -> Vec<(u32, FirCaptureSource)> {
    Walk {
        declaration,
        checked,
        bodies: Vec::new(),
    }
    .closure(function)
}

struct Walk<'a> {
    declaration: BodyLocalCallableDeclarationId,
    checked: &'a HashMap<BodyLocalCallableDeclarationId, Box<[FirCapture]>>,
    /// The function's body, then each lambda being walked inside it.
    bodies: Vec<&'a FirBody>,
}

impl<'a> Walk<'a> {
    /// The values `body` reads from outside itself, each keyed relative to its parent: those it
    /// reads directly, then those its callees, lambdas and anonymous objects include.
    fn closure(&mut self, body: &'a FirBody) -> Vec<(u32, FirCaptureSource)> {
        self.bodies.push(body);
        let mut read = Vec::new();
        let mut included = Vec::new();
        for raw in 0..body.expression_count() {
            let id = FirExprId::from_raw(u32::try_from(raw).expect("too many FIR expressions"));
            let Some(expression) = body.expr(id) else {
                continue;
            };
            match &expression.kind {
                FirExprKind::CapturedValueRead {
                    enclosing_depth,
                    source,
                }
                | FirExprKind::CapturedValueWrite {
                    enclosing_depth,
                    source,
                    ..
                } => read.push((*enclosing_depth, FirCaptureSource::Value(*source))),
                FirExprKind::LocalCall { target, .. }
                | FirExprKind::LocalCallableReference { target, .. } => {
                    self.include(target, &mut included)
                }
                // A lambda's closure is relative to this body: what it reads from this body's
                // parent and above is this body's.
                FirExprKind::Lambda { body, .. } => {
                    included.extend(
                        self.closure(body)
                            .into_iter()
                            .filter_map(|(depth, source)| Some((depth.checked_sub(1)?, source))),
                    );
                }
                FirExprKind::AnonymousObject(object) => {
                    included.extend(object.captures.iter().filter_map(
                        |capture| match capture.source {
                            FirLocalClassCaptureSource::Captured {
                                enclosing_depth,
                                source,
                            } => Some((enclosing_depth, FirCaptureSource::Value(source))),
                            _ => None,
                        },
                    ));
                }
                _ => {}
            }
        }
        self.bodies.pop();
        read.extend(included);
        read
    }

    /// Include the captures of the local function `target` names from the innermost body.
    fn include(&self, target: &FirLocalCallableRef, included: &mut Vec<(u32, FirCaptureSource)>) {
        // A target outside the streamed body receives its captures as explicit operands.
        if target.external_capture_arguments.is_some() {
            return;
        }
        let Some(callee) = target
            .declaration
            .filter(|callee| *callee != self.declaration)
        else {
            return;
        };
        let innermost = self.bodies.len() - 1;
        let captures = match innermost.checked_sub(target.body_depth as usize) {
            Some(declaring) => declared_captures(self.bodies[declaring], callee)
                .expect("a local function is declared before the body that calls it"),
            None => match self.checked.get(&callee) {
                Some(captures) => captures,
                // A function enclosing this one is still being checked; its captures are ordered
                // after this one's.
                None => return,
            },
        };
        // A callee capture is relative to the body declaring the callee, `body_depth` above the
        // innermost one; it is one of the innermost body's when it lies above that body.
        for capture in captures {
            let above = target.body_depth + capture.enclosing_depth;
            if let Some(depth) = above.checked_sub(1) {
                included.push((depth, capture.source));
            }
        }
    }
}

/// The captures of the local function `callee` that `body` declares.
fn declared_captures(
    body: &FirBody,
    callee: BodyLocalCallableDeclarationId,
) -> Option<&[FirCapture]> {
    (0..body.statement_count()).find_map(|raw| {
        let id = FirStatementId::from_raw(u32::try_from(raw).expect("too many FIR statements"));
        match &body.statement(id)?.kind {
            FirStatementKind::LocalFunction {
                declaration, body, ..
            } if *declaration == callee => Some(body.captures()),
            _ => None,
        }
    })
}
