//! Value-parameter lists.
//!
//! One parameter list is parsed the same way wherever it is written — a function, a secondary
//! constructor, an accessor — so the modifiers a parameter may carry are read in exactly one
//! place.

use super::*;

impl Parser<'_> {
    /// Parse `(a: A, vararg b: B, noinline c: () -> Unit = …)`, positioned on the `(`.
    pub(super) fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.expect(TokenKind::LParen, "'('");
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let prefix_start = self.i;
            let mut pmods = Vec::new();
            let mut pannos = Vec::new();
            let mut pannos_args = Vec::new();
            // `value` is a valid parameter name in Kotlin; only collect real parameter modifiers. A modifier
            // soft keyword used as a NAME (`fun f(open: Int)`) is left for the name parse below —
            // `skip_decl_prefix` stops before a modifier-ident that is immediately followed by `:`.
            if self.at(TokenKind::At) || (self.at_modifier() && self.text() != "value") {
                pmods = self.skip_decl_prefix(); // `@Anno`, `vararg`, `noinline`, … on a parameter
                pannos = self.take_pending_annotations();
                pannos_args = self.take_pending_annotation_args();
            }
            let is_vararg = pmods.iter().any(|m| m == "vararg");
            let vararg_span = is_vararg.then(|| {
                self.t[prefix_start..self.i]
                    .iter()
                    .find(|token| self.token_keyword_text(**token, "vararg"))
                    .map(|token| token.span)
                    .expect("a vararg parameter must retain its source modifier")
            });
            // `noinline` is the declaration's own statement that this argument is a real closure
            // rather than a body spliced at each use. Nothing downstream can recover it from the
            // parameter's TYPE: a `noinline` parameter is function-typed exactly like the spliced
            // one beside it.
            //
            // `crossinline` is NOT this. It forbids a non-local return from the lambda, and the
            // reference compiler still inlines the body, so such a parameter owns no local either.
            let is_materialized_lambda = pmods.iter().any(|modifier| modifier == "noinline");
            let pname = if self.at(TokenKind::Ident) {
                let n = self.text().to_string();
                self.bump();
                n
            } else {
                self.diags.error(self.tok().span, "expected parameter name");
                "<error>".to_string()
            };
            self.expect(TokenKind::Colon, "':'");
            let ty = self.parse_type();
            let default_operator = self.eat_span(TokenKind::Eq);
            let default = if default_operator.is_some() {
                self.skip_newlines();
                Some(self.parse_expr())
            } else {
                None
            };
            if let (Some(operator), Some(default)) = (default_operator, default) {
                self.file.value_operator_spans.insert(default.0, operator);
            }
            params.push(Param {
                name: pname,
                ty,
                is_vararg,
                vararg_span,
                is_materialized_lambda,
                default,
                annotations: pannos,
                annotation_args: pannos_args,
            });
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        params
    }
}
