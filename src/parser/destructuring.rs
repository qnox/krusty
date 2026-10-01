//! Destructuring declarations and the parser-owned syntax they retain for later phases.

use super::*;

impl Parser<'_> {
    /// A full-form destructuring statement starts with `(` (name-based) or `[` (positional, only
    /// under `+NameBasedDestructuring`) immediately followed by a `val`/`var` keyword.
    pub(super) fn at_full_form_destructure(&self) -> bool {
        if !self.name_based_destructuring {
            return false;
        }
        let opener = self.at(TokenKind::LParen) || self.at(TokenKind::LBracket);
        opener
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| matches!(token.kind, TokenKind::KwVal | TokenKind::KwVar))
    }

    /// Parse a full-form destructuring declaration: `(val a, val b) = e` / `[var a, var b] = e`.
    /// Each component carries its own mutability and optional type/source-property declaration.
    pub(super) fn parse_full_form_destructure(&mut self, start: Span) -> StmtId {
        let close = if self.at(TokenKind::LParen) {
            TokenKind::RParen
        } else {
            TokenKind::RBracket
        };
        self.bump();
        let mut entries = Vec::new();
        let mut source_properties = Vec::new();
        let mut entry_types = Vec::new();
        loop {
            let is_var = self.at(TokenKind::KwVar);
            if is_var || self.at(TokenKind::KwVal) {
                self.bump();
            } else {
                self.diags
                    .error(self.tok().span, "expected 'val' or 'var'".to_string());
            }
            let ignored = self.at(TokenKind::Ident) && self.text() == "_" && !self.escaped_ident();
            let name = self.ident_or_error("variable name");
            let mut entry_type = self.eat(TokenKind::Colon).then(|| self.parse_type());
            let source_property = if self.eat(TokenKind::Eq) {
                let source = self.ident_or_error("property name");
                if self.eat(TokenKind::Colon) {
                    entry_type = Some(self.parse_type());
                }
                Some(source)
            } else if close == TokenKind::RParen {
                Some(name.clone())
            } else {
                None
            };
            entries.push(DestructureEntry {
                name,
                mutable: is_var,
                ignored,
            });
            source_properties.push(source_property);
            entry_types.push(entry_type);
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.at(close) {
                break;
            }
        }
        self.expect(
            close,
            if close == TokenKind::RParen {
                "')'"
            } else {
                "']'"
            },
        );
        self.expect(TokenKind::Eq, "'='");
        self.skip_newlines();
        let init = self.parse_expr();
        let statement = self.finish_stmt(Stmt::Destructure { entries, init }, start);
        self.record_destructure_syntax(statement, source_properties, entry_types);
        statement
    }

    /// Parse a local `val`/`var`, including the leading-keyword destructuring form.
    pub(super) fn parse_local_binding(&mut self, start: Span) -> StmtId {
        let is_var = self.at(TokenKind::KwVar);
        self.bump();
        let close = if self.at(TokenKind::LParen) {
            Some(TokenKind::RParen)
        } else if self.at(TokenKind::LBracket) {
            self.note_ungated_bracket_destructure();
            Some(TokenKind::RBracket)
        } else {
            None
        };
        if let Some(close) = close {
            self.bump();
            let mut entries = Vec::new();
            let mut source_properties = Vec::new();
            let mut entry_types = Vec::new();
            loop {
                let ignored =
                    self.at(TokenKind::Ident) && self.text() == "_" && !self.escaped_ident();
                let name = self.ident_or_error("variable name");
                let mut entry_type = self.eat(TokenKind::Colon).then(|| self.parse_type());
                let source_property = if self.name_based_destructuring && self.eat(TokenKind::Eq) {
                    let source = self.ident_or_error("property name");
                    if self.eat(TokenKind::Colon) {
                        entry_type = Some(self.parse_type());
                    }
                    Some(source)
                } else if self.short_form_destructuring && close == TokenKind::RParen {
                    Some(name.clone())
                } else {
                    None
                };
                entries.push(DestructureEntry {
                    name,
                    mutable: is_var,
                    ignored,
                });
                source_properties.push(source_property);
                entry_types.push(entry_type);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.at(close) {
                    break;
                }
            }
            self.expect(
                close,
                if close == TokenKind::RParen {
                    "')'"
                } else {
                    "']'"
                },
            );
            self.skip_plain_newlines_before(TokenKind::Eq);
            self.expect(TokenKind::Eq, "'='");
            self.skip_newlines();
            let init = self.parse_expr();
            let statement = self.finish_stmt(Stmt::Destructure { entries, init }, start);
            self.record_destructure_syntax(statement, source_properties, entry_types);
            return statement;
        }

        let name = self.ident_or_error("variable name");
        let ty = if self.eat(TokenKind::Colon) {
            self.skip_plain_newlines();
            Some(self.parse_type())
        } else {
            None
        };
        if self.at(TokenKind::Ident) && self.keyword_text("by") {
            let by_span = self.tok().span;
            self.bump();
            self.skip_newlines();
            let delegate = self.parse_unlabelled_expr();
            return self.finish_stmt(
                Stmt::LocalDelegate {
                    is_var,
                    name,
                    ty,
                    delegate,
                    by_span,
                },
                start,
            );
        }

        let initializer = self.eat_initializer_eq();
        let deferred = ty.is_some() && initializer.is_none();
        let init_operator = (!deferred).then(|| {
            initializer.unwrap_or_else(|| {
                let operator = self.tok().span;
                self.expect(TokenKind::Eq, "'='");
                operator
            })
        });
        let init = if deferred {
            let span = self.tok().span;
            self.default_init_expr(ty.as_ref().unwrap(), span)
        } else {
            self.skip_newlines();
            self.parse_unlabelled_expr()
        };
        if let Some(operator) = init_operator {
            self.file.value_operator_spans.insert(init.0, operator);
        }
        self.finish_stmt(
            Stmt::Local {
                is_var: is_var || deferred,
                name,
                ty,
                init,
            },
            start,
        )
    }

    /// Square-bracket destructuring is parsed at every language level. The `[` span is retained so
    /// the frontend can report the language-version diagnostic after module admission.
    pub(super) fn note_ungated_bracket_destructure(&mut self) {
        if !self.name_based_destructuring {
            self.file
                .destructuring
                .ungated_bracket_spans
                .push(self.tok().span);
        }
    }

    fn record_destructure_syntax(
        &mut self,
        statement: StmtId,
        source_properties: Vec<Option<String>>,
        entry_types: Vec<Option<TypeRef>>,
    ) {
        if source_properties.iter().any(Option::is_some) {
            self.file
                .destructuring
                .source_properties
                .insert(statement.0, source_properties);
        }
        if entry_types.iter().any(Option::is_some) {
            self.file
                .destructuring
                .entry_types
                .insert(statement.0, entry_types);
        }
    }
}
