//! Kotlin scripts. kotlinc compiles a script to one class named after its file: the script's
//! top-level declarations become the class's members, its statements run in source order in the
//! primary constructor, and the class extends the script definition's template. The parser builds
//! that class directly, so every later phase sees an ordinary classifier.

use super::*;
use crate::ast::{AccessorLines, ClassInit, Decl, Expr, PropDecl, PropParam, Stmt};
use crate::script_definitions::{ScriptDefinition, ScriptParameter};

/// The script class kotlinc names after `file_stem`, the file name without `.kts`:
/// `build.gradle` becomes `Build_gradle` and `1abc` becomes `_1abc`.
pub fn script_class_name(file_stem: &str) -> String {
    let mut name: String = file_stem
        .chars()
        .map(|character| {
            if character == '_' || character == '$' || character.is_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    match name.chars().next() {
        Some(first) if first.is_ascii_lowercase() => {
            name.replace_range(..1, &first.to_ascii_uppercase().to_string());
        }
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {}
        _ => name.insert(0, '_'),
    }
    name
}

impl Parser<'_> {
    /// Parse everything after a script's file header as the body of the script class `name` and
    /// publish that class. Like kotlinc's, the script starts at the last token of its file header
    /// (file annotations, package directive and imports). Without a header it starts with the
    /// file, or after its shebang line. Its constructor delegation is placed there.
    pub(super) fn parse_script_class(&mut self, name: &str, definition: &ScriptDefinition) {
        let script_start = self.t[..self.i]
            .iter()
            .rev()
            .find(|token| token.kind != TokenKind::Newline)
            .map_or_else(|| self.script_text_start(), |token| token.span.lo);
        let declaration_index = self.file.decls.len();
        let start = Span::new(script_start, script_start);
        let mut methods = Vec::new();
        let mut body_props = Vec::new();
        let mut init_order = Vec::new();
        let mut type_aliases = Vec::new();
        // The statement the script ends with, if no declaration follows it.
        let mut trailing_statement = None;
        loop {
            self.skip_newlines();
            if self.at(TokenKind::Eof) {
                break;
            }
            if self.at_file_annotation() {
                self.diags.error(
                    self.tok().span,
                    "file annotations must precede script statements",
                );
                self.parse_annotation();
                continue;
            }
            if matches!(self.kind(), TokenKind::KwPackage | TokenKind::KwImport) {
                self.diags.error(
                    self.tok().span,
                    "package and import directives must precede script statements",
                );
                // Keep the directive whole so its name is not read as a statement.
                if self.at(TokenKind::KwImport) {
                    self.parse_import_directive();
                } else {
                    self.bump();
                    self.parse_qualified_name();
                }
                continue;
            }
            trailing_statement = None;
            if self.at_script_member_declaration() {
                let modifiers = self.parse_member_decl_prefix();
                if self.parse_and_register_nested_classifier(name, &modifiers, true) {
                    continue;
                }
                match self.kind() {
                    TokenKind::KwFun => methods.push(self.parse_fun(&modifiers)),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let property = self.parse_member_property(&modifiers, true, false);
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(property);
                    }
                    _ => type_aliases.push(self.parse_type_alias_syntax()),
                }
                continue;
            }
            let statement = self.parse_stmt();
            let span = self.file.stmt_spans[statement.0 as usize];
            trailing_statement = Some(statement);
            let block = self.file.add_expr(
                Expr::Block {
                    stmts: vec![statement],
                    trailing: None,
                },
                span,
            );
            init_order.push(ClassInit::Block(block));
        }
        self.file.script_last_statement = trailing_statement;
        let trailing_expression =
            trailing_statement.and_then(|statement| match self.file.stmt(statement) {
                Stmt::Expr(expression) => {
                    Some((*expression, self.file.stmt_spans[statement.0 as usize]))
                }
                _ => None,
            });
        if let Some((expression, span)) = trailing_expression {
            // kotlinc keeps the value of the script's last expression in the `$$result` property.
            // The checker types its initializer as the statement it is, and lowering keeps no
            // property for a value of type `Unit` or `Nothing`.
            init_order.pop();
            init_order.push(ClassInit::PropInit(body_props.len()));
            body_props.push(script_result_property(expression, span));
            self.file.script_result = Some(expression);
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        let mut class = class_recovery::error_class_decl(Span::new(start.lo, end.hi.max(start.lo)));
        class.name = name.to_string();
        class.name_span = Span::new(start.lo, start.lo);
        class.methods = methods;
        class.body_props = body_props;
        class.init_order = init_order;
        class.type_aliases = type_aliases;
        class.props = definition
            .constructor_parameters
            .iter()
            .map(script_constructor_parameter)
            .collect();
        class.base_class = definition.template.clone();
        class.base_args = definition
            .constructor_parameters
            .iter()
            .map(|parameter| {
                self.file.add_expr(
                    Expr::Name(parameter.name.clone()),
                    Span::new(start.lo, start.lo),
                )
            })
            .collect();
        let id = self.file.add_decl(Decl::Class(class));
        self.file.decls.insert(declaration_index, id);
        self.file
            .adopt_hoisted_classifiers(id, declaration_index + 1);
        self.file.script_class = Some(id);
    }

    /// Where a script without a file header starts: at the file's start, or at the first
    /// non-blank character after its shebang line.
    fn script_text_start(&self) -> u32 {
        let bytes = self.src.as_bytes();
        if !bytes.starts_with(b"#!") {
            return 0;
        }
        let mut start = 0;
        while start < bytes.len() && !matches!(bytes[start], b'\r' | b'\n') {
            start += 1;
        }
        while start < bytes.len() && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        u32::try_from(start).expect("source offsets fit in u32")
    }

    /// Whether the cursor starts a script member declaration rather than a statement: a function,
    /// property, classifier or type alias, after any annotations and modifiers.
    fn at_script_member_declaration(&self) -> bool {
        if self.local_declaration_after_prefix().is_some() || self.looks_like_local_type_decl() {
            return true;
        }
        let mut index = self.i;
        while self
            .t
            .get(index)
            .is_some_and(|token| self.token_is_modifier(*token))
        {
            index += 1;
        }
        self.t.get(index).is_some_and(|token| {
            (self.token_keyword_text(*token, "object")
                && self
                    .t
                    .get(index + 1)
                    .is_some_and(|next| next.kind == TokenKind::Ident))
                || self.token_keyword_text(*token, "typealias")
        })
    }
}

/// The `$$result` property holding the value of the script's last expression.
fn script_result_property(initializer: ExprId, span: Span) -> PropDecl {
    PropDecl {
        name: "$$result".to_string(),
        annotations: Vec::new(),
        annotation_args: Vec::new(),
        context_params: Vec::new(),
        decl_line: 0,
        accessor_lines: AccessorLines::default(),
        visibility: Visibility::Public,
        has_visibility_modifier: false,
        type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        reified_type_params: Default::default(),
        receiver: None,
        ty: None,
        is_var: false,
        is_companion_extension: false,
        is_companion_block_member: false,
        is_open: false,
        is_final: false,
        is_override: false,
        override_span: None,
        init: Some(initializer),
        is_lateinit: false,
        is_external: false,
        is_expect: false,
        is_actual: false,
        getter: None,
        getter_declared: false,
        getter_span: None,
        getter_declaration_start: 0,
        getter_inline: false,
        getter_ty: None,
        getter_annotations: Vec::new(),
        getter_reads_field: false,
        setter: None,
        is_const: false,
        is_abstract: false,
        delegate: None,
        delegate_by_span: None,
        explicit_backing_field: None,
        span,
        declaration_span: span,
        name_span: Span::new(span.lo, span.lo),
    }
}

/// A script constructor parameter. It is passed to the template's constructor and is not in scope
/// in the script body, where the same name is the template's member.
fn script_constructor_parameter(parameter: &ScriptParameter) -> PropParam {
    let zero = Span::new(0, 0);
    let mut ty = simple_type_ref(&parameter.classifier, zero);
    ty.targs = parameter
        .type_arguments
        .iter()
        .map(|argument| simple_type_ref(argument, zero))
        .collect();
    PropParam {
        name: parameter.name.clone(),
        ty,
        span: zero,
        declaration_span: zero,
        decl_line: 0,
        decl_start_line: 0,
        is_vararg: false,
        is_var: false,
        is_property: false,
        is_script_parameter: true,
        is_actual: false,
        is_override: false,
        has_open_modifier: false,
        is_final: false,
        is_open: false,
        visibility: Visibility::Public,
        has_visibility_modifier: false,
        default: None,
        annotations: Vec::new(),
        annotation_args: Vec::new(),
    }
}
