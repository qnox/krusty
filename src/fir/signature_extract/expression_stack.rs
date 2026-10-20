//! Stack growth for the recursive expression walk.
//!
//! Pre-inference extracts an inferred signature's expression body before any per-file check
//! runs, so this walk recurses as deep as the checker does and must survive the same nesting
//! bound on a small embedder thread.

use super::{ExpressionForm, OriginId, SigExprId, SignatureConstraintExtractor, SignatureScopeId};
use crate::ast::{ExprId, File};

/// How many nested expressions the walk descends between checks of the remaining stack. Checking
/// before the first frame and then every 64 nested frames keeps the largest unchecked run well
/// below one grown segment, without paying `stacker`'s probe for every ordinary AST node.
const EXPRESSION_STACK_CHECK_INTERVAL: u32 = 64;

impl SignatureConstraintExtractor {
    pub(super) fn expression(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        self.expression_depth = self
            .expression_depth
            .checked_add(1)
            .expect("expression nesting exceeds u32");
        let check_stack = self.expression_depth == 1
            || self
                .expression_depth
                .is_multiple_of(EXPRESSION_STACK_CHECK_INTERVAL);
        let result = if check_stack {
            crate::wide_stack::on_wide_stack(|| {
                self.expression_frame(file, expression, scope, origin)
            })
        } else {
            self.expression_frame(file, expression, scope, origin)
        };
        self.expression_depth -= 1;
        result
    }
}
