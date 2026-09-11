//! Shared recursion bounds and recovery for nested parser constructs.

use super::*;

impl Parser<'_> {
    /// Apply the parser's depth, recovery, and stack-growth contract to every recursive funnel.
    ///
    /// Keeping the counter lane, diagnostic label, delimiter policy, and per-level stack growth in
    /// one operation prevents a new syntax origin from silently receiving a different safety
    /// policy. Callers provide only their typed degraded node; that is the one aspect which cannot
    /// be shared between an expression, type, statement, and declaration AST.
    pub(super) fn with_nesting_guard<T>(
        &mut self,
        nesting: ParserNesting,
        degraded: impl FnOnce(&mut Self, crate::diag::Span) -> T,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if *nesting.counter_mut(self) >= EXPR_DEPTH_LIMIT {
            let span = self.tok().span;
            self.diags
                .error(span, format!("{} nesting too deep", nesting.label()));
            self.skip_over_deep_rest(nesting.angle_brackets());
            return degraded(self, span);
        }

        *nesting.counter_mut(self) += 1;
        // Check/grow at EVERY recursive level. A single grown segment was measured to fail before
        // the bound because each semantic level stacks several large unoptimized parser frames.
        // `maybe_grow` is cheap while enough stack remains and chains a segment only near its
        // low-water mark.
        let result = crate::wide_stack::on_wide_stack(|| parse(self));
        *nesting.counter_mut(self) -= 1;
        result
    }

    /// Recovery for a tripped nesting guard: skip the REST of the over-deep construct,
    /// bracket-balanced. Without this, the leftover tokens (`((((…`) re-parse as nesting on the
    /// error node, rebuilding a deep tree that the downstream passes then recurse over — and a
    /// closer-per-frame unwind would emit an `expected ')'`/`'}'` cascade. Stop at a closer or
    /// newline at relative depth 0 (they belong to the enclosing construct, whose frame consumes
    /// them on the way out) or at EOF. With `angle_brackets`, `<`/`>` count as brackets too — in
    /// TYPE position they always are, so each enclosing `parse_type_args` frame finds exactly the
    /// one `>` it expects. A fused `>=` token (`List<Int>= e`, no space) is skipped as "other",
    /// losing a closer — exact parity with the normal-depth parser, whose `parse_type_args` also
    /// fails to see a `>` inside `GtEq`.
    fn skip_over_deep_rest(&mut self, angle_brackets: bool) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                TokenKind::Eof => break,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::Lt if angle_brackets => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                }
                TokenKind::Gt if angle_brackets => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                }
                TokenKind::Newline if depth == 0 => break,
                _ => {
                    self.bump();
                }
            }
        }
    }
}
