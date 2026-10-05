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
            let name = (self.at(TokenKind::Ident)
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| t.kind == TokenKind::Colon))
            .then(|| {
                let name = self.text().to_string();
                self.bump(); // name
                self.bump(); // ':'
                name
            });
            let parameter = self.parse_type();
            if let Some(name) = name {
                self.file
                    .function_type_parameter_names
                    .insert(parameter.span.lo, name);
            }
            params.push(parameter);
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

#[cfg(test)]
mod tests {
    use crate::ast::Decl;
    use crate::diag::DiagSink;
    use crate::lexer::lex;

    use super::super::parse;

    fn parsed_parameter(source: &str) -> crate::ast::TypeRef {
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            diagnostics.diags.is_empty(),
            "{}",
            diagnostics.render("test.kt", source)
        );
        let Decl::Fun(function) = file.decl(file.decls[0]) else {
            panic!("expected a top-level function");
        };
        function.params[0].ty.clone()
    }

    fn parameter_names(source: &str) -> Vec<String> {
        parsed_parameter(source)
            .fun_params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect()
    }

    #[test]
    fn parenthesized_receiver_is_an_extension_function_type() {
        let source = "fun f(a: (Receiver).() -> Result) {}";
        let ty = parsed_parameter(source);
        assert!(ty.fun_has_receiver());
        assert!(!ty.fun_suspend());
        assert_eq!(ty.fun_context_count, 0);
        assert_eq!(parameter_names(source), ["Receiver"]);
        assert_eq!(ty.arg.unwrap().name, "Result");
    }

    #[test]
    fn nullable_parenthesized_receiver_stays_the_receiver() {
        let ty = parsed_parameter("fun f(a: (Receiver)?.() -> Result) {}");
        assert!(ty.fun_has_receiver());
        assert!(ty.fun_params[0].nullable());
        assert_eq!(ty.fun_params[0].name, "Receiver");
    }

    #[test]
    fn suspend_parenthesized_receiver_keeps_suspend() {
        let source = "fun f(a: suspend (Receiver).() -> Result) {}";
        let ty = parsed_parameter(source);
        assert!(ty.fun_suspend());
        assert!(ty.fun_has_receiver());
        assert_eq!(parameter_names(source), ["Receiver"]);
        assert_eq!(ty.arg.unwrap().name, "Result");
    }

    #[test]
    fn context_on_a_parenthesized_receiver_precedes_it() {
        let source = "fun f(a: context(Context) (Receiver).() -> Result) {}";
        let ty = parsed_parameter(source);
        assert!(ty.fun_has_receiver());
        assert_eq!(ty.fun_context_count, 1);
        assert_eq!(parameter_names(source), ["Context", "Receiver"]);
        assert_eq!(ty.arg.unwrap().name, "Result");
    }

    #[test]
    fn context_parenthesized_receiver_keeps_value_parameters() {
        let source = "fun f(a: context(Context) (Receiver).(First, Second) -> Result) {}";
        let ty = parsed_parameter(source);
        assert!(ty.fun_has_receiver());
        assert_eq!(ty.fun_context_count, 1);
        assert_eq!(
            parameter_names(source),
            ["Context", "Receiver", "First", "Second"]
        );
        assert_eq!(ty.arg.unwrap().name, "Result");
    }

    #[test]
    fn parenthesized_function_type_can_be_a_receiver() {
        let ty = parsed_parameter("fun f(a: ((Argument) -> Value).() -> Result) {}");
        assert!(ty.fun_has_receiver());
        assert_eq!(ty.fun_context_count, 0);
        assert_eq!(ty.fun_params.len(), 1);
        assert_eq!(ty.fun_params[0].name, "<fun>");
        assert_eq!(ty.fun_params[0].fun_params[0].name, "Argument");
        assert_eq!(ty.fun_params[0].arg.as_ref().unwrap().name, "Value");
        assert_eq!(ty.arg.unwrap().name, "Result");
    }

    #[test]
    fn identifier_receiver_still_precedes_context_parameters() {
        let source = "fun f(a: context(Context) Receiver.(Argument) -> Result) {}";
        let ty = parsed_parameter(source);
        assert!(ty.fun_has_receiver());
        assert_eq!(ty.fun_context_count, 1);
        assert_eq!(parameter_names(source), ["Context", "Receiver", "Argument"]);
    }

    #[test]
    fn grouping_parentheses_are_not_a_receiver() {
        let ty = parsed_parameter("fun f(a: (Value)) {}");
        assert_eq!(ty.name, "Value");
        assert!(ty.fun_params.is_empty());
        assert!(!ty.fun_has_receiver());

        let nullable = parsed_parameter("fun f(a: (Value)?) {}");
        assert_eq!(nullable.name, "Value");
        assert!(nullable.nullable());

        let function = parsed_parameter("fun f(a: (() -> Result)?) {}");
        assert_eq!(function.name, "<fun>");
        assert!(function.nullable());
        assert!(!function.fun_has_receiver());
        assert!(function.fun_params.is_empty());
    }

    #[test]
    fn anonymous_function_receiver_is_not_a_function_type() {
        let source = "fun f() { val y = fun Receiver.() = this }";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let _ = parse(source, &tokens, &mut diagnostics);
        assert!(
            diagnostics.diags.is_empty(),
            "{}",
            diagnostics.render("test.kt", source)
        );
    }

    #[test]
    fn context_without_a_function_type_is_rejected() {
        let source = "fun f(a: context(Context) (Value)) {}";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let _ = parse(source, &tokens, &mut diagnostics);
        let messages = diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, ["expected '->' for function type"]);
    }
}
