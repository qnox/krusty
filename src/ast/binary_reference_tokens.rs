//! The tokens kotlinc reads to place a diagnostic on a binary expression.
//!
//! kotlinc locates a reference on a binary expression (`REFERENCED_NAME_BY_QUALIFIED` under the
//! default light-tree front end) by searching the expression's tokens, not by its shape: an
//! augmented-assignment token anywhere inside it is marked itself; otherwise a `=` token inside it,
//! outside any nested function declaration, marks its first operand, parentheses included; only
//! then is the operator token marked. A named argument (`f(flag = true) + 1`) or a lambda holding
//! an assignment therefore moves the report off the operator. The parser keeps exactly the token
//! facts that search reads, so no source text or token stream outlives the parse.

use crate::diag::Span;
use crate::token::{Token, TokenKind};

#[derive(Clone, Debug, Default)]
pub struct BinaryReferenceTokens {
    /// Start offset of each `=` token, ascending.
    assignments: Vec<u32>,
    /// Each `+=`, `-=`, `*=`, `/=` and `%=` token, ascending.
    augmented: Vec<Span>,
    /// Start offset of each `(` token, ascending.
    opening_parens: Vec<u32>,
    /// Each `)` token, ascending.
    closing_parens: Vec<Span>,
    /// Each function declaration and anonymous function, from its `fun` keyword to its end.
    functions: Vec<Span>,
}

impl BinaryReferenceTokens {
    pub(crate) fn new(tokens: &[Token], mut functions: Vec<Span>) -> Self {
        let spans_of = |kinds: &[TokenKind]| -> Vec<Span> {
            tokens
                .iter()
                .filter(|token| kinds.contains(&token.kind))
                .map(|token| token.span)
                .collect()
        };
        let starts = |spans: Vec<Span>| spans.into_iter().map(|span| span.lo).collect();
        functions.sort_by_key(|function| (function.lo, function.hi));
        functions.dedup();
        Self {
            assignments: starts(spans_of(&[TokenKind::Eq])),
            augmented: spans_of(&[
                TokenKind::PlusEq,
                TokenKind::MinusEq,
                TokenKind::StarEq,
                TokenKind::SlashEq,
                TokenKind::PercentEq,
            ]),
            opening_parens: starts(spans_of(&[TokenKind::LParen])),
            closing_parens: spans_of(&[TokenKind::RParen]),
            functions,
        }
    }

    /// Where kotlinc reports a reference on the binary expression `node`, whose first operand
    /// spans `first_operand` inside any parentheses and whose operator token is `operator`. Of
    /// several augmented assignments inside it, the first in source order is marked.
    pub fn binary_reference_span(&self, node: Span, first_operand: Span, operator: Span) -> Span {
        let within = |offset: u32| node.lo <= offset && offset < node.hi;
        let from = self.augmented.partition_point(|token| token.lo < node.lo);
        if let Some(token) = self.augmented[from..].first().filter(|t| within(t.lo)) {
            return *token;
        }
        let from = self.assignments.partition_point(|offset| *offset < node.lo);
        let assigns = self.assignments[from..]
            .iter()
            .take_while(|offset| within(**offset))
            .any(|offset| !self.in_nested_function(node, *offset));
        if assigns {
            self.parenthesized(first_operand, operator)
        } else {
            operator
        }
    }

    /// Whether a function declared inside `node` encloses `offset`.
    fn in_nested_function(&self, node: Span, offset: u32) -> bool {
        let from = self
            .functions
            .partition_point(|function| function.lo < node.lo);
        self.functions[from..]
            .iter()
            .take_while(|function| function.lo < offset)
            .any(|function| offset < function.hi)
    }

    /// `operand` with the parentheses wrapping it: each `)` between it and the operator token
    /// closes one of the `(` tokens directly before it.
    fn parenthesized(&self, operand: Span, operator: Span) -> Span {
        let from = self
            .closing_parens
            .partition_point(|paren| paren.lo < operand.hi);
        let Some(last) = self.closing_parens[from..]
            .iter()
            .take_while(|paren| paren.hi <= operator.lo)
            .enumerate()
            .last()
        else {
            return operand;
        };
        let (closed, paren) = (last.0 + 1, last.1);
        let before = self
            .opening_parens
            .partition_point(|offset| *offset < operand.lo);
        match before.checked_sub(closed) {
            Some(first) => Span::new(self.opening_parens[first], paren.hi),
            None => operand,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;

    fn marked(source: &str, first: &str, functions: &[&str]) -> Span {
        let tokens = crate::lexer::lex(source, &mut DiagSink::new());
        let functions = functions.iter().map(|text| span_of(source, text)).collect();
        BinaryReferenceTokens::new(&tokens, functions).binary_reference_span(
            span_of(source, source),
            span_of(source, first),
            span_of(source, "+"),
        )
    }

    fn span_of(source: &str, text: &str) -> Span {
        let lo = source.find(text).expect("fragment") as u32;
        Span::new(lo, lo + text.len() as u32)
    }

    #[test]
    fn a_binary_without_assignments_marks_its_operator() {
        let source = "f(a == b) + 2";
        assert_eq!(marked(source, "f(a == b)", &[]), span_of(source, "+"));
    }

    #[test]
    fn a_named_argument_marks_the_first_operand() {
        let source = "1 + f(flag = true)";
        assert_eq!(marked(source, "1", &[]), span_of(source, "1"));
    }

    #[test]
    fn the_marked_first_operand_keeps_its_parentheses() {
        let source = "( (f(flag = true)) ) + 1";
        let marked = marked(source, "f(flag = true)", &[]);
        assert_eq!(marked, span_of(source, "( (f(flag = true)) )"));
    }

    #[test]
    fn an_assignment_inside_a_nested_function_is_skipped() {
        let source = "1 + (fun(): Int { val z = 2; return z })()";
        let function = "fun(): Int { val z = 2; return z }";
        assert_eq!(marked(source, "1", &[function]), span_of(source, "+"));
    }

    #[test]
    fn an_augmented_assignment_marks_itself() {
        let source = "1 + run { var k = 0; k += 2; k }";
        assert_eq!(marked(source, "1", &[]), span_of(source, "+="));
    }
}
