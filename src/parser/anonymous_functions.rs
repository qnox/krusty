//! Anonymous function expressions (`fun (x: T): R = …`), which desugar to an `Expr::Lambda`
//! marked as an anonymous function, with side tables for what a lambda literal cannot say.

use super::*;

impl Parser<'_> {
    /// Anonymous function expression: `fun (params): T = expr` / `fun (params): T { … }`. Desugars to a
    /// lambda (`Expr::Lambda`) carrying each parameter's declared type in the `lambda_param_types`
    /// side-table, so the value types even without an expected function type. An expression body
    /// (`= expr`) becomes a `Block` whose only value is that expression; a block body reuses the normal
    /// statement parser, so a `return` inside returns from the anonymous function (it lowers to the
    /// lambda's own `invoke`). A receiver form `fun R.(…)` records `R` separately and uses the same
    /// semantic path as any other receiver lambda.
    pub(super) fn parse_anon_fun(&mut self, context_params: Vec<Param>) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'fun'
        let receiver = if self.at(TokenKind::LParen) {
            None
        } else {
            self.parsing_anonymous_function_receiver = true;
            let receiver = self.parse_type();
            self.parsing_anonymous_function_receiver = false;
            self.expect(TokenKind::Dot, "'.'");
            Some(receiver)
        };
        self.expect(TokenKind::LParen, "'('");
        let context_count = context_params.len() as u32;
        let mut params: Vec<String> = context_params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect();
        let mut param_types: Vec<Option<TypeRef>> = context_params
            .into_iter()
            .map(|parameter| Some(parameter.ty))
            .collect();
        // Context parameters were parsed before this expression and are always explicitly typed;
        // only value-parameter spans can own expectation-free inference diagnostics here.
        let mut param_spans = vec![start; params.len()];
        // A context parameter's anonymity is its context kind; the roles describe value parameters.
        let mut roles = vec![LambdaParameterRole::Named; params.len()];
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let parameter_span = self.tok().span;
            // `_` marks an unused parameter; keep the name so the arity is preserved.
            roles.push(
                if self.at(TokenKind::Ident) && self.text() == "_" && !self.escaped_ident() {
                    LambdaParameterRole::Unused
                } else {
                    LambdaParameterRole::Named
                },
            );
            let name = self.ident_or_error("parameter name");
            let ty = if self.eat(TokenKind::Colon) {
                Some(self.parse_type())
            } else {
                None
            };
            params.push(name);
            param_spans.push(parameter_span);
            param_types.push(ty);
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        // An explicit return type (`: T`) drives the desugared lambda's function type — recorded below
        // once the lambda ExprId exists. A block body ending in `return` has body type `Nothing`, so the
        // checker relies on this annotation rather than the (diverging) body value.
        let ret_ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let body = if self.eat(TokenKind::Eq) {
            let e = self.parse_expr();
            let sp = self.file.expr_spans[e.0 as usize];
            self.file.add_expr(
                Expr::Block {
                    stmts: Vec::new(),
                    trailing: Some(e),
                },
                sp,
            )
        } else {
            self.parse_block_expr(false)
        };
        let end = self.file.expr_spans[body.0 as usize];
        let lam = self
            .file
            .add_expr(Expr::Lambda { params, body }, Span::new(start.lo, end.hi));
        if param_types.iter().any(|t| t.is_some()) {
            self.file.lambda_param_types.insert(lam.0, param_types);
        }
        if !param_spans.is_empty() {
            self.file.lambda_param_spans.insert(lam.0, param_spans);
        }
        if roles.iter().any(|role| *role != LambdaParameterRole::Named) {
            self.file.lambda_parameter_roles.insert(lam.0, roles);
        }
        self.file.anon_fun_lambdas.insert(lam.0);
        if context_count != 0 {
            self.file
                .anon_fun_context_count
                .insert(lam.0, context_count);
        }
        if let Some(receiver) = receiver {
            self.file.anon_fun_receivers.insert(lam.0, receiver);
        }
        if let Some(rt) = ret_ty {
            self.file.anon_fun_ret.insert(lam.0, rt);
        }
        lam
    }
}
