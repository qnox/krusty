//! Type alias declarations. The syntax is the same at file, classifier, and local scope; only a
//! local one is gated, by `LocalTypeAliases`.

use super::*;

impl Parser<'_> {
    /// Parse the complete declaration-shaped part of a type alias. Semantic registration differs
    /// between file, classifier, and local scopes, but no scope is allowed to skip its tokens.
    pub(super) fn parse_type_alias_syntax(&mut self) -> crate::ast::TypeAliasDecl {
        let start = self.tok().span;
        let declaration_start = match self.member_declaration_prefix {
            Some((prefix_start, prefix_end)) if prefix_end == self.i => prefix_start,
            _ => start.lo,
        };
        self.gate_type_alias(Span::new(declaration_start, start.hi));
        self.bump(); // `typealias`
        let name = self.ident_or_error("typealias name");
        let name_span = self.declaration_name_span;
        let type_params = if self.at(TokenKind::Lt) {
            self.parse_type_params(start.lo).0
        } else {
            Vec::new()
        };
        self.expect(TokenKind::Eq, "'='");
        self.skip_plain_newlines();
        let target = self.parse_type();
        let end = self.t[self.i.saturating_sub(1)].span;
        crate::ast::TypeAliasDecl {
            name,
            type_params,
            target,
            span: Span::new(start.lo, end.hi),
            name_span,
        }
    }
}
