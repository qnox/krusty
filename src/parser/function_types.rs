//! Parsing of function-type shapes that follow a type's head: the parenthesized parameter list of
//! `(A, B) -> R` and the extension tail `Recv.(A) -> R`.
//!
//! An extension function type's receiver is any type, not only a named one: kotlinc's grammar is
//! `functionType: (receiverType '.')? functionTypeParameters '->' type`, where `receiverType` may be
//! a parenthesized type (`(String).() -> R`, `(() -> Unit)?.() -> R`). Both the named-receiver and the
//! parenthesized-receiver heads therefore share one tail parser here.

use super::{Parser, TokenKind};
use crate::ast::{TrFlags, TypeRef};
use crate::diag::Span;

impl Parser<'_> {
    /// Parse `( [name:] Type, … )` of a function type — also a parenthesized grouping `(Type)` when
    /// no `->` follows. The cursor is on the `(`.
    pub(super) fn parse_function_type_parameters(&mut self) -> Vec<TypeRef> {
        self.bump(); // '('
        self.skip_newlines();
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            // Optional parameter name `name: Type`.
            if self.at(TokenKind::Ident)
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| t.kind == TokenKind::Colon)
            {
                self.bump(); // name
                self.bump(); // ':'
            }
            params.push(self.parse_type());
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.skip_newlines();
        self.expect(TokenKind::RParen, "')'");
        params
    }

    /// Whether the cursor is on the `.(` that turns the type just parsed into an extension function
    /// type's receiver. An anonymous function's own receiver (`fun Recv.(…)`) is not a type tail.
    pub(super) fn at_extension_function_type_tail(&self) -> bool {
        !self.parsing_anonymous_function_receiver
            && self.at(TokenKind::Dot)
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::LParen)
    }

    /// Parse `.(A, …) -> R` after `receiver`. The receiver folds in as the first function parameter
    /// after any context receivers, exactly how Kotlin lowers an extension function type to
    /// `FunctionN`, so the rest of the pipeline sees `(Ctx…, Recv, A…) -> R`.
    pub(super) fn parse_extension_function_type(
        &mut self,
        receiver: TypeRef,
        context_types: Vec<TypeRef>,
        fun_suspend: bool,
        span: Span,
    ) -> TypeRef {
        self.bump(); // '.'
        let fun_context_count = context_types.len() as u32;
        let mut fun_params = context_types;
        fun_params.push(receiver);
        fun_params.extend(self.parse_function_type_parameters());
        self.expect(TokenKind::Arrow, "'->'");
        let ret = self.parse_type();
        let nullable = self.eat_type_nullable();
        TypeRef {
            name: "<fun>".to_string(),
            flags: TrFlags::default()
                .with_nullable(nullable)
                .with_definitely_non_null(false)
                .with_fun_has_receiver(true)
                .with_fun_suspend(fun_suspend),
            arg: Some(Box::new(ret)),
            targs: Vec::new(),
            span,
            fun_params,
            fun_context_count,
        }
    }

    /// The type a malformed type reference parses to after its diagnostic was reported.
    pub(super) fn error_type_ref(span: Span) -> TypeRef {
        TypeRef {
            name: "<error>".to_string(),
            flags: TrFlags::default()
                .with_nullable(false)
                .with_definitely_non_null(false),
            arg: None,
            targs: Vec::new(),
            span,
            fun_params: Vec::new(),
            fun_context_count: 0,
        }
    }
}
