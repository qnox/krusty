//! Parsing of a declaration's `context(…)` clause.
//!
//! A context clause precedes the declaration it belongs to and lowers to LEADING value parameters
//! (kotlinc's ABI), so the parser buffers it and the following function or property prepends it.
//! Keeping that here rather than in the parser facade also keeps the clause's own source span with
//! the code that reads it: a diagnostic about the clause is anchored on the `context` keyword, not
//! on whichever modifier happens to follow.

use super::{Param, Parser, TokenKind};

impl Parser<'_> {
    /// Whether the first entry in a `context(...)` clause has a top-level name/type separator.
    /// Looking only at the first token misclassifies a named parameter preceded by `noinline`,
    /// `crossinline`, `vararg`, or an annotation as a legacy unnamed context receiver.
    fn context_clause_has_named_parameters(&self, open: usize) -> bool {
        let mut parentheses = 0usize;
        let mut brackets = 0usize;
        let mut braces = 0usize;
        for token in self.t.iter().skip(open + 1) {
            match token.kind {
                TokenKind::LParen => parentheses += 1,
                TokenKind::RParen if parentheses != 0 => parentheses -= 1,
                TokenKind::RParen if brackets == 0 && braces == 0 => return false,
                TokenKind::LBracket => brackets += 1,
                TokenKind::RBracket => brackets = brackets.saturating_sub(1),
                TokenKind::LBrace => braces += 1,
                TokenKind::RBrace => braces = braces.saturating_sub(1),
                TokenKind::Colon if parentheses == 0 && brackets == 0 && braces == 0 => {
                    return true;
                }
                TokenKind::Comma if parentheses == 0 && brackets == 0 && braces == 0 => {
                    return false;
                }
                TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }

    /// Buffer a `context(...)` clause for the following function or property.
    pub(super) fn maybe_parse_context_receivers(&mut self) -> Vec<String> {
        if !(self.at(TokenKind::Ident)
            && self.keyword_text("context")
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::LParen))
        {
            return Vec::new();
        }
        let context_span = self.tok().span;
        self.pending_context_span = Some(context_span);
        self.bump(); // 'context'
        let named_parameters = self.context_clause_has_named_parameters(self.i);
        if named_parameters {
            if !self.context_parameters {
                self.diags.error(
                    context_span,
                    "the feature 'context parameters' is disabled".to_string(),
                );
            }
            self.pending_context_params = self.parse_param_list();
            for parameter in &mut self.pending_context_params {
                parameter.context_kind = if parameter.name == "_" {
                    crate::ast::ContextParameterKind::Anonymous
                } else {
                    crate::ast::ContextParameterKind::Named
                };
            }
        } else {
            if !self.context_receivers {
                self.diags.error(
                    context_span,
                    "the feature 'context receivers' is disabled".to_string(),
                );
            }
            self.pending_context_params = self.parse_context_receiver_list();
        }
        self.skip_newlines();
        // Modifiers/annotations may follow the context prefix (`context(a: A) private fun …`);
        // consume them so the declaration keyword is next, and RETURN them so the caller keeps the
        // visibility/modality (annotations buffer as pending, read by the declaration parser).
        if self.at(TokenKind::At) || self.at_modifier() {
            let m = self.skip_decl_prefix();
            self.skip_newlines();
            m
        } else {
            Vec::new()
        }
    }

    /// Parse legacy `context(A, B)` receivers into unnamed leading semantic parameters. Their
    /// declaration context count retains receiver behavior; `_` prevents a value binding for a
    /// receiver that has no source parameter name.
    fn parse_context_receiver_list(&mut self) -> Vec<Param> {
        let mut receivers = Vec::new();
        self.expect(TokenKind::LParen, "'('");
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            receivers.push(Param {
                name: "_".to_owned(),
                ty: self.parse_type(),
                context_kind: crate::ast::ContextParameterKind::LegacyReceiver,
                is_vararg: false,
                vararg_span: None,
                is_materialized_lambda: false,
                default: None,
                annotations: Vec::new(),
                annotation_args: Vec::new(),
            });
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        receivers
    }
}
