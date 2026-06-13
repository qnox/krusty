//! Stage B: tokens → arena AST. Recursive descent for decls/stmts, Pratt for expressions with the
//! Kotlin precedence table. Newlines (their own token) act as statement/expression terminators;
//! they are skipped after binary operators and between statements/declarations.

use crate::ast::*;
use crate::diag::{DiagSink, Span};
use crate::token::{Token, TokenKind};

pub fn parse(src: &str, tokens: &[Token], diags: &mut DiagSink) -> File {
    let mut p = Parser { src, t: tokens, i: 0, file: File::default(), diags };
    p.parse_file();
    p.file
}

struct Parser<'a> {
    src: &'a str,
    t: &'a [Token],
    i: usize,
    file: File,
    diags: &'a mut DiagSink,
}

impl<'a> Parser<'a> {
    // ---- cursor helpers ----
    fn kind(&self) -> TokenKind {
        self.t[self.i].kind
    }
    fn tok(&self) -> Token {
        self.t[self.i]
    }
    fn text(&self) -> &'a str {
        self.t[self.i].text(self.src)
    }
    fn at(&self, k: TokenKind) -> bool {
        self.kind() == k
    }
    fn bump(&mut self) -> Token {
        let t = self.t[self.i];
        if self.i + 1 < self.t.len() {
            self.i += 1;
        }
        t
    }
    fn eat(&mut self, k: TokenKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, k: TokenKind, what: &str) -> bool {
        if self.eat(k) {
            true
        } else {
            self.diags.error(self.tok().span, format!("expected {what}"));
            false
        }
    }
    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.bump();
        }
    }

    // ---- file / decls ----
    fn parse_file(&mut self) {
        self.skip_newlines();
        if self.at(TokenKind::KwPackage) {
            self.bump();
            self.file.package = Some(self.parse_qualified_name());
        }
        loop {
            self.skip_newlines();
            if self.at(TokenKind::Eof) {
                break;
            }
            // Consume leading annotations + declaration modifiers. `open`/`abstract` are applied to
            // the following class; the rest are ignored (krusty treats everything as public).
            let mods = if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                let m = self.skip_decl_prefix();
                self.skip_newlines();
                m
            } else {
                Vec::new()
            };
            // A `sealed` class is implicitly abstract and open (subclasses live in the same module).
            let is_sealed = mods.iter().any(|m| m == "sealed");
            let is_open = is_sealed || mods.iter().any(|m| m == "open");
            let is_abstract = is_sealed || mods.iter().any(|m| m == "abstract");
            match self.kind() {
                TokenKind::Eof => break,
                TokenKind::KwImport => {
                    self.bump(); // 'import'
                    let fq = self.parse_qualified_name();
                    if !fq.is_empty() {
                        self.file.imports.push(fq);
                    }
                    // tolerate trailing tokens (e.g. `as alias`) to end of line
                    while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                        self.bump();
                    }
                }
                TokenKind::KwFun => {
                    let d = self.parse_fun();
                    let id = self.file.add_decl(Decl::Fun(d));
                    self.file.decls.push(id);
                }
                TokenKind::KwClass => {
                    let mut d = self.parse_class();
                    d.is_open = is_open;
                    d.is_abstract = is_abstract;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // top-level property: `val`/`var name (: Type)? = init`
                TokenKind::KwVal | TokenKind::KwVar => {
                    let d = self.parse_top_property();
                    let id = self.file.add_decl(Decl::Property(d));
                    self.file.decls.push(id);
                }
                // `data class` — `data` is a soft keyword (a plain identifier elsewhere).
                TokenKind::Ident
                    if self.text() == "data" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass) =>
                {
                    self.bump(); // 'data'
                    let mut d = self.parse_class();
                    d.is_data = true;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `object Name { … }` — a singleton (soft keyword `object` + a name).
                TokenKind::Ident
                    if self.text() == "object" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident) =>
                {
                    let d = self.parse_object();
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `enum class Name { A, B, C }` (soft keyword `enum` + `class`).
                TokenKind::Ident
                    if self.text() == "enum" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass) =>
                {
                    let d = self.parse_enum();
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `interface Name { … }` (soft keyword `interface` + a name).
                TokenKind::Ident
                    if self.text() == "interface" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident) =>
                {
                    let d = self.parse_interface();
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                _ => {
                    self.diags.error(self.tok().span, "expected a top-level declaration");
                    self.bump(); // recover
                }
            }
        }
    }

    /// Consume leading annotations (`@Foo`, `@file:Bar(...)`) and soft modifiers (`public`, `open`,
    /// `inline`, `operator`, `suspend`, …) that precede a declaration. Modifiers that change the
    /// declaration *kind* (`enum`, `annotation`, `sealed`, `data`, `value`, `object`, …) are NOT
    /// skipped, so those declarations remain unsupported (and the file is cleanly skipped).
    fn skip_decl_prefix(&mut self) -> Vec<String> {
        let mut mods = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::At) {
                self.skip_annotation();
            } else if self.at(TokenKind::Ident) && is_modifier(self.text()) {
                mods.push(self.text().to_string());
                self.bump();
            } else {
                break;
            }
        }
        mods
    }

    fn skip_annotation(&mut self) {
        self.bump(); // '@'
        // optional use-site target: `file:`, `get:`, `param:`, ...
        if self.at(TokenKind::Ident) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Colon) {
            self.bump();
            self.bump(); // ':'
        }
        let _ = self.parse_qualified_name();
        if self.at(TokenKind::LParen) {
            // skip a balanced argument list
            let mut depth = 0;
            loop {
                match self.kind() {
                    TokenKind::LParen => { depth += 1; self.bump(); }
                    TokenKind::RParen => { depth -= 1; self.bump(); if depth == 0 { break; } }
                    TokenKind::Eof => break,
                    _ => { self.bump(); }
                }
            }
        }
    }

    fn parse_top_property(&mut self) -> PropDecl {
        let start = self.tok().span;
        let is_var = self.at(TokenKind::KwVar);
        self.bump(); // val/var
        let name = self.ident_or_error("property name");
        let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        self.expect(TokenKind::Eq, "'='"); // v0: top-level properties require an initializer
        self.skip_newlines();
        let init = self.parse_expr();
        let end = self.t[self.i.saturating_sub(1)].span;
        PropDecl { name, ty, is_var, init, span: Span::new(start.lo, end.hi) }
    }

    /// `enum class Name { A, B, C }` — v0: simple entries (no constructor args, no class body).
    fn parse_enum(&mut self) -> ClassDecl {
        let start = self.tok().span;
        self.bump(); // 'enum'
        self.bump(); // 'class'
        let name = self.ident_or_error("enum name");
        let mut entries = Vec::new();
        self.skip_newlines();
        if self.eat(TokenKind::LBrace) {
            self.skip_newlines();
            while self.at(TokenKind::Ident) {
                entries.push(self.text().to_string());
                self.bump();
                self.skip_newlines();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            self.skip_newlines();
            // v0: only simple entries; a `;` + members or entry args make it unsupported.
            if !self.at(TokenKind::RBrace) {
                self.diags.error(self.tok().span, "v0 enum: only simple entries are supported");
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl {
            name,
            type_params: Vec::new(),
            props: Vec::new(),
            methods: Vec::new(),
            body_props: Vec::new(),
            init_order: Vec::new(),
            is_data: false,
            is_object: false,
            is_enum: true,
            enum_entries: entries,
            is_interface: false,
            is_open: false,
            is_abstract: false,
            supertypes: Vec::new(),
            base_class: None,
            base_args: Vec::new(),
            span: Span::new(start.lo, end.hi),
        }
    }

    fn parse_qualified_name(&mut self) -> String {
        let mut s = String::new();
        if self.at(TokenKind::Ident) {
            s.push_str(self.text());
            self.bump();
            while self.at(TokenKind::Dot) {
                self.bump();
                if self.at(TokenKind::Ident) {
                    s.push('.');
                    s.push_str(self.text());
                    self.bump();
                }
            }
        }
        s
    }

    fn parse_fun(&mut self) -> FunDecl {
        let start = self.tok().span;
        self.bump(); // 'fun'
        let type_params = if self.at(TokenKind::Lt) { self.parse_type_params() } else { Vec::new() };
        let name = if self.at(TokenKind::Ident) {
            let n = self.text().to_string();
            self.bump();
            n
        } else {
            self.diags.error(self.tok().span, "expected function name");
            "<error>".to_string()
        };
        let mut params = Vec::new();
        self.expect(TokenKind::LParen, "'('");
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                self.skip_decl_prefix(); // `@Anno`, `vararg`, etc. on a parameter
            }
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
            params.push(Param { name: pname, ty });
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        let ret = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let body = if self.eat(TokenKind::Eq) {
            self.skip_newlines();
            FunBody::Expr(self.parse_expr())
        } else if self.at(TokenKind::LBrace) {
            FunBody::Block(self.parse_block_expr())
        } else {
            FunBody::None
        };
        let end = self.t[self.i.saturating_sub(1)].span;
        FunDecl { name, params, ret, body, type_params, span: Span::new(start.lo, end.hi) }
    }

    /// v0 class: `class Name(val/var p: Type, ...)` with an optional empty body `{}`.
    /// Every primary-constructor parameter must be a `val`/`var` property (no plain params yet).
    fn parse_class(&mut self) -> ClassDecl {
        let start = self.tok().span;
        self.bump(); // 'class'
        let name = if self.at(TokenKind::Ident) {
            let n = self.text().to_string();
            self.bump();
            n
        } else {
            self.diags.error(self.tok().span, "expected class name");
            "<error>".to_string()
        };
        let type_params = if self.at(TokenKind::Lt) { self.parse_type_params() } else { Vec::new() };
        let mut props = Vec::new();
        if self.eat(TokenKind::LParen) {
            self.skip_newlines();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    self.skip_decl_prefix(); // `private val x`, `@Anno val y`, ...
                }
                let (is_property, is_var) = match self.kind() {
                    TokenKind::KwVal => { self.bump(); (true, false) }
                    TokenKind::KwVar => { self.bump(); (true, true) }
                    _ => (false, false), // a plain constructor parameter (not a property)
                };
                let pname = self.ident_or_error("parameter name");
                self.expect(TokenKind::Colon, "':'");
                let ty = self.parse_type();
                props.push(PropParam { name: pname, ty, is_var, is_property });
                self.skip_newlines();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            self.expect(TokenKind::RParen, "')'");
        }
        // Optional supertype list: `: Iface1, Base(args), Iface2`. Supertypes with `()` are the
        // base class (v0: unsupported → flagged); the rest are implemented interfaces.
        let (supertypes, base_class, base_args) = self.parse_supertypes();
        // Optional class body: member `fun`s, body properties (`val`/`var`), and `init { }` blocks.
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    self.skip_decl_prefix();
                    self.skip_newlines();
                }
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun()),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let p = self.parse_top_property();
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident if self.text() == "init" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::LBrace) => {
                        self.bump(); // 'init'
                        let block = self.parse_block_expr();
                        init_order.push(ClassInit::Block(block));
                    }
                    _ => {
                        self.diags.error(self.tok().span, "v0: class bodies support member 'fun', 'val'/'var', and 'init' blocks");
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl { name, type_params, props, methods, body_props, init_order, is_data: false, is_object: false, is_enum: false, enum_entries: Vec::new(), is_interface: false, is_open: false, is_abstract: false, supertypes, base_class, base_args, span: Span::new(start.lo, end.hi) }
    }

    /// Parse an optional `: Base(args), Iface1, Iface2` supertype list. A supertype with `()` is the
    /// base class (returns its name + ctor-arg expressions); the rest are implemented interfaces.
    fn parse_supertypes(&mut self) -> (Vec<String>, Option<String>, Vec<ExprId>) {
        let mut ifaces = Vec::new();
        let mut base: Option<String> = None;
        let mut base_args = Vec::new();
        if self.eat(TokenKind::Colon) {
            loop {
                self.skip_newlines();
                let name = self.parse_qualified_name();
                let simple = name.rsplit('.').next().unwrap_or(&name).to_string();
                if self.eat(TokenKind::LParen) {
                    // constructor call → base class
                    self.skip_newlines();
                    let mut args = Vec::new();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        args.push(self.parse_expr());
                        self.skip_newlines();
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                    self.expect(TokenKind::RParen, "')'");
                    base = Some(simple);
                    base_args = args;
                } else if !simple.is_empty() {
                    ifaces.push(simple);
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }
        (ifaces, base, base_args)
    }

    /// `interface Name { fun sig(): T }` — abstract member functions only (v0).
    fn parse_interface(&mut self) -> ClassDecl {
        let start = self.tok().span;
        self.bump(); // 'interface'
        let name = self.ident_or_error("interface name");
        let type_params = if self.at(TokenKind::Lt) { self.parse_type_params() } else { Vec::new() };
        let (supertypes, _base, _base_args) = self.parse_supertypes();
        let mut methods = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    self.skip_decl_prefix();
                    self.skip_newlines();
                }
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun()),
                    _ => {
                        self.diags.error(self.tok().span, "v0: interface bodies support only abstract 'fun' declarations");
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl {
            name, type_params, props: Vec::new(), methods, body_props: Vec::new(), init_order: Vec::new(),
            is_data: false, is_object: false, is_enum: false,
            enum_entries: Vec::new(), is_interface: true, is_open: false, is_abstract: false,
            supertypes, base_class: None, base_args: Vec::new(),
            span: Span::new(start.lo, end.hi),
        }
    }

    /// `object Name { fun … }` — a singleton with member functions (no primary constructor).
    fn parse_object(&mut self) -> ClassDecl {
        let start = self.tok().span;
        self.bump(); // 'object'
        let name = self.ident_or_error("object name");
        let mut methods = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    self.skip_decl_prefix();
                    self.skip_newlines();
                }
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun()),
                    _ => {
                        self.diags.error(self.tok().span, "v0: object bodies support only member 'fun' declarations");
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl { name, type_params: Vec::new(), props: Vec::new(), methods, body_props: Vec::new(), init_order: Vec::new(), is_data: false, is_object: true, is_enum: false, enum_entries: Vec::new(), is_interface: false, is_open: false, is_abstract: false, supertypes: Vec::new(), base_class: None, base_args: Vec::new(), span: Span::new(start.lo, end.hi) }
    }

    fn parse_type(&mut self) -> TypeRef {
        let span = self.tok().span;
        if self.at(TokenKind::Ident) {
            let name = self.text().to_string();
            self.bump();
            self.skip_type_args(); // erase generic type arguments: `Box<Int>` → raw `Box`
            let nullable = self.eat(TokenKind::Question); // `T?`
            TypeRef { name, nullable, span }
        } else {
            self.diags.error(span, "expected a type");
            TypeRef { name: "<error>".to_string(), nullable: false, span }
        }
    }

    /// Skip a balanced `<...>` generic type-argument list (types are erased).
    fn skip_type_args(&mut self) {
        if !self.at(TokenKind::Lt) {
            return;
        }
        let mut depth = 0;
        loop {
            match self.kind() {
                TokenKind::Lt => { depth += 1; self.bump(); }
                TokenKind::Gt => { depth -= 1; self.bump(); if depth == 0 { break; } }
                TokenKind::Eof => break,
                _ => { self.bump(); }
            }
        }
    }

    /// Parse and discard a `<T, reified U : Bound, out V>` type-parameter list, returning the names.
    fn parse_type_params(&mut self) -> Vec<String> {
        let mut names = Vec::new();
        if !self.eat(TokenKind::Lt) {
            return names;
        }
        loop {
            self.skip_newlines();
            while self.at(TokenKind::Ident) && matches!(self.text(), "reified" | "out" | "in") {
                self.bump();
            }
            if self.at(TokenKind::Ident) {
                names.push(self.text().to_string());
                self.bump();
            }
            if self.eat(TokenKind::Colon) {
                let _ = self.parse_type(); // upper bound (erased)
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "'>'");
        names
    }

    // ---- statements ----
    fn parse_block_expr(&mut self) -> ExprId {
        let start = self.tok().span;
        self.expect(TokenKind::LBrace, "'{'");
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            stmts.push(self.parse_stmt());
        }
        let end = self.tok().span;
        self.expect(TokenKind::RBrace, "'}'");
        // A trailing bare expression is the block's value.
        let mut trailing = None;
        if let Some(&last) = stmts.last() {
            if let Stmt::Expr(e) = self.file.stmt(last) {
                trailing = Some(*e);
                stmts.pop();
            }
        }
        self.file.add_expr(Expr::Block { stmts, trailing }, Span::new(start.lo, end.hi))
    }

    fn parse_stmt(&mut self) -> StmtId {
        let start = self.tok().span;
        match self.kind() {
            TokenKind::KwVal | TokenKind::KwVar => {
                let is_var = self.at(TokenKind::KwVar);
                self.bump();
                let name = self.ident_or_error("variable name");
                let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
                self.expect(TokenKind::Eq, "'='");
                self.skip_newlines();
                let init = self.parse_expr();
                self.finish_stmt(Stmt::Local { is_var, name, ty, init }, start)
            }
            TokenKind::KwReturn => {
                self.bump();
                let e = if self.at(TokenKind::Newline) || self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                    None
                } else {
                    Some(self.parse_expr())
                };
                self.finish_stmt(Stmt::Return(e), start)
            }
            TokenKind::KwWhile => {
                self.bump();
                self.expect(TokenKind::LParen, "'('");
                let cond = self.parse_expr();
                self.expect(TokenKind::RParen, "')'");
                self.skip_newlines();
                let body = if self.at(TokenKind::LBrace) {
                    self.parse_block_expr()
                } else {
                    self.parse_expr()
                };
                self.finish_stmt(Stmt::While { cond, body }, start)
            }
            TokenKind::KwFor => self.parse_for(start),
            _ => {
                let e = self.parse_expr();
                // assignment: `name = value` (v0: simple-name target only)
                if self.at(TokenKind::Eq) {
                    if let Expr::Name(n) = self.file.expr(e) {
                        let name = n.clone();
                        self.bump(); // '='
                        self.skip_newlines();
                        let value = self.parse_expr();
                        return self.finish_stmt(Stmt::Assign { name, value }, start);
                    }
                    self.diags.error(self.tok().span, "invalid assignment target");
                }
                // compound assignment: `name op= value` → `name = name op value`.
                if let Some(op) = compound_op(self.kind()) {
                    if let Expr::Name(n) = self.file.expr(e).clone() {
                        let name = n.clone();
                        let op_span = self.tok().span;
                        self.bump();
                        self.skip_newlines();
                        let rhs = self.parse_expr();
                        let lhs = self.file.add_expr(Expr::Name(name.clone()), op_span);
                        let value = self.file.add_expr(Expr::Binary { op, lhs, rhs }, op_span);
                        return self.finish_stmt(Stmt::Assign { name, value }, start);
                    }
                    self.diags.error(self.tok().span, "invalid assignment target");
                }
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    fn parse_for(&mut self, start: Span) -> StmtId {
        self.bump(); // 'for'
        self.expect(TokenKind::LParen, "'('");
        let name = self.ident_or_error("loop variable");
        self.expect(TokenKind::KwIn, "'in'");
        let rstart = self.parse_expr();
        let kind = if self.eat(TokenKind::DotDot) {
            RangeKind::Through
        } else if self.at(TokenKind::Ident) && self.text() == "until" {
            self.bump();
            RangeKind::Until
        } else if self.at(TokenKind::Ident) && self.text() == "downTo" {
            self.bump();
            RangeKind::DownTo
        } else {
            self.diags.error(self.tok().span, "v0 `for` supports only `a..b`, `a until b`, `a downTo b`");
            RangeKind::Through
        };
        let rend = self.parse_expr();
        let step = if self.at(TokenKind::Ident) && self.text() == "step" {
            self.bump();
            Some(self.parse_expr())
        } else {
            None
        };
        self.expect(TokenKind::RParen, "')'");
        self.skip_newlines();
        let body = self.parse_branch();
        self.finish_stmt(Stmt::For { name, range: ForRange { start: rstart, end: rend, kind, step }, body }, start)
    }

    fn finish_stmt(&mut self, s: Stmt, start: Span) -> StmtId {
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_stmt(s, Span::new(start.lo, end.hi))
    }

    fn ident_or_error(&mut self, what: &str) -> String {
        if self.at(TokenKind::Ident) {
            let n = self.text().to_string();
            self.bump();
            n
        } else {
            self.diags.error(self.tok().span, format!("expected {what}"));
            "<error>".to_string()
        }
    }

    // ---- expressions (Pratt) ----
    fn parse_expr(&mut self) -> ExprId {
        self.parse_elvis()
    }

    /// Elvis `?:` is the lowest-precedence binary operator (below `||`).
    fn parse_elvis(&mut self) -> ExprId {
        let mut lhs = self.parse_bp(0);
        while self.at(TokenKind::Question) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Colon) {
            self.bump(); // '?'
            self.bump(); // ':'
            self.skip_newlines();
            let rhs = self.parse_bp(0);
            let lspan = self.file.expr_spans[lhs.0 as usize];
            let rspan = self.file.expr_spans[rhs.0 as usize];
            lhs = self.file.add_expr(Expr::Elvis { lhs, rhs }, Span::new(lspan.lo, rspan.hi));
        }
        lhs
    }

    fn parse_bp(&mut self, min_bp: u8) -> ExprId {
        let mut lhs = self.parse_prefix();
        loop {
            // `is` / `!is` type test — a "named check" at comparison precedence (binding power 7).
            if min_bp <= 7 {
                let negated = if self.at(TokenKind::Ident) && self.text() == "is" {
                    Some(false)
                } else if self.at(TokenKind::Not)
                    && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident && t.text(self.src) == "is")
                {
                    Some(true)
                } else {
                    None
                };
                if let Some(negated) = negated {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    if negated {
                        self.bump(); // '!'
                    }
                    self.bump(); // 'is'
                    let ty = self.parse_type();
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(Expr::Is { operand: lhs, ty, negated }, Span::new(lspan.lo, end.hi));
                    continue;
                }
            }
            let op = match infix_op(self.kind()) {
                Some(o) => o,
                None => break,
            };
            let (lbp, rbp) = infix_bp(op);
            if lbp < min_bp {
                break;
            }
            let op_span = self.tok().span;
            self.bump();
            self.skip_newlines();
            let rhs = self.parse_bp(rbp);
            let lspan = self.file.expr_spans[lhs.0 as usize];
            let rspan = self.file.expr_spans[rhs.0 as usize];
            lhs = self.file.add_expr(Expr::Binary { op, lhs, rhs }, Span::new(lspan.lo, rspan.hi));
            let _ = op_span;
        }
        lhs
    }

    fn parse_prefix(&mut self) -> ExprId {
        let start = self.tok().span;
        let unop = match self.kind() {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Not => Some(UnOp::Not),
            _ => None,
        };
        if let Some(op) = unop {
            self.bump();
            let operand = self.parse_bp(BP_PREFIX);
            let end = self.file.expr_spans[operand.0 as usize];
            return self.file.add_expr(Expr::Unary { op, operand }, Span::new(start.lo, end.hi));
        }
        let primary = self.parse_primary();
        self.parse_postfix(primary)
    }

    fn parse_postfix(&mut self, mut lhs: ExprId) -> ExprId {
        loop {
            // `as T` / `as? T` cast — binds tighter than the binary operators (postfix level).
            if self.at(TokenKind::Ident) && self.text() == "as" {
                let lspan = self.file.expr_spans[lhs.0 as usize];
                self.bump(); // 'as'
                let nullable = self.eat(TokenKind::Question);
                let ty = self.parse_type();
                let end = self.t[self.i.saturating_sub(1)].span;
                lhs = self.file.add_expr(Expr::As { operand: lhs, ty, nullable }, Span::new(lspan.lo, end.hi));
                continue;
            }
            match self.kind() {
                // `!!` not-null assertion in postfix position = two consecutive `Not` tokens.
                TokenKind::Not if self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Not) => {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump();
                    let end = self.tok().span;
                    self.bump();
                    lhs = self.file.add_expr(Expr::NotNull { operand: lhs }, Span::new(lspan.lo, end.hi));
                }
                // `?.` safe call: `recv?.name` or `recv?.name(args)`.
                TokenKind::Question if self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Dot) => {
                    self.bump(); // '?'
                    self.bump(); // '.'
                    let name = self.ident_or_error("member name");
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let args = if self.at(TokenKind::LParen) {
                        self.bump();
                        self.skip_newlines();
                        let mut args = Vec::new();
                        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                            args.push(self.parse_expr());
                            self.skip_newlines();
                            if !self.eat(TokenKind::Comma) {
                                break;
                            }
                            self.skip_newlines();
                        }
                        self.expect(TokenKind::RParen, "')'");
                        Some(args)
                    } else {
                        None
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(Expr::SafeCall { receiver: lhs, name, args }, Span::new(lspan.lo, end.hi));
                }
                TokenKind::Dot => {
                    self.bump();
                    let name = self.ident_or_error("member name");
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(Expr::Member { receiver: lhs, name }, Span::new(lspan.lo, end.hi));
                }
                TokenKind::LParen => {
                    self.bump();
                    self.skip_newlines();
                    let mut args = Vec::new();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        args.push(self.parse_expr());
                        self.skip_newlines();
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.expect(TokenKind::RParen, "')'");
                    lhs = self.file.add_expr(Expr::Call { callee: lhs, args }, Span::new(lspan.lo, end.hi));
                }
                _ => break,
            }
        }
        lhs
    }

    fn parse_primary(&mut self) -> ExprId {
        let span = self.tok().span;
        match self.kind() {
            TokenKind::IntLit => {
                let v = self.text().parse::<i64>().unwrap_or(0);
                self.bump();
                self.file.add_expr(Expr::IntLit(v), span)
            }
            TokenKind::LongLit => {
                let t = self.text();
                let v = t[..t.len() - 1].parse::<i64>().unwrap_or(0);
                self.bump();
                self.file.add_expr(Expr::LongLit(v), span)
            }
            TokenKind::DoubleLit => {
                let v = self.text().parse::<f64>().unwrap_or(0.0);
                self.bump();
                self.file.add_expr(Expr::DoubleLit(v), span)
            }
            TokenKind::StringLit => {
                let raw = self.text();
                let v = unquote(raw);
                self.bump();
                self.file.add_expr(Expr::StringLit(v), span)
            }
            TokenKind::CharLit => {
                let raw = self.text();
                let c = unquote_char(raw);
                self.bump();
                self.file.add_expr(Expr::CharLit(c), span)
            }
            TokenKind::TemplateStart => self.parse_template(),
            TokenKind::KwTrue => {
                self.bump();
                self.file.add_expr(Expr::BoolLit(true), span)
            }
            TokenKind::KwFalse => {
                self.bump();
                self.file.add_expr(Expr::BoolLit(false), span)
            }
            TokenKind::KwNull => {
                self.bump();
                self.file.add_expr(Expr::NullLit, span)
            }
            TokenKind::Ident => {
                let n = self.text().to_string();
                self.bump();
                self.file.add_expr(Expr::Name(n), span)
            }
            TokenKind::LParen => {
                self.bump();
                self.skip_newlines();
                let e = self.parse_expr();
                self.skip_newlines();
                self.expect(TokenKind::RParen, "')'");
                e
            }
            TokenKind::KwIf => self.parse_if(),
            TokenKind::KwWhen => self.parse_when(),
            TokenKind::LBrace => self.parse_block_expr(),
            _ => {
                self.diags.error(span, "expected an expression");
                self.bump();
                self.file.add_expr(Expr::Name("<error>".to_string()), span)
            }
        }
    }

    fn parse_if(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'if'
        self.expect(TokenKind::LParen, "'('");
        let cond = self.parse_expr();
        self.expect(TokenKind::RParen, "')'");
        self.skip_newlines();
        let then_branch = self.parse_branch();
        // optional else (may be on the next line)
        let save = self.i;
        self.skip_newlines();
        let else_branch = if self.eat(TokenKind::KwElse) {
            self.skip_newlines();
            Some(self.parse_branch())
        } else {
            self.i = save;
            None
        };
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(Expr::If { cond, then_branch, else_branch }, Span::new(start.lo, end.hi))
    }

    /// Parse a string template: `TemplateStart (StrChunk | Dollar Ident | Dollar { expr })* TemplateEnd`.
    fn parse_template(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // TemplateStart
        let mut parts = Vec::new();
        loop {
            match self.kind() {
                TokenKind::StrChunk => {
                    parts.push(TemplatePart::Str(unescape_chunk(self.text())));
                    self.bump();
                }
                TokenKind::Dollar => {
                    self.bump();
                    if self.eat(TokenKind::LBrace) {
                        let e = self.parse_expr();
                        self.expect(TokenKind::RBrace, "'}'");
                        parts.push(TemplatePart::Expr(e));
                    } else if self.at(TokenKind::Ident) {
                        let sp = self.tok().span;
                        let n = self.text().to_string();
                        self.bump();
                        let e = self.file.add_expr(Expr::Name(n), sp);
                        parts.push(TemplatePart::Expr(e));
                    }
                }
                TokenKind::TemplateEnd => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                _ => {
                    self.bump(); // recover
                }
            }
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(Expr::Template(parts), Span::new(start.lo, end.hi))
    }

    fn parse_when(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'when'
        let subject = if self.eat(TokenKind::LParen) {
            let e = self.parse_expr();
            self.expect(TokenKind::RParen, "')'");
            Some(e)
        } else {
            None
        };
        self.skip_newlines();
        self.expect(TokenKind::LBrace, "'{'");
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            let mut conditions = Vec::new();
            if self.eat(TokenKind::KwElse) {
                // else arm — no conditions
            } else {
                conditions.push(self.parse_expr());
                while self.eat(TokenKind::Comma) {
                    self.skip_newlines();
                    conditions.push(self.parse_expr());
                }
            }
            self.expect(TokenKind::Arrow, "'->'");
            self.skip_newlines();
            let body = self.parse_branch();
            arms.push(WhenArm { conditions, body });
        }
        let end = self.tok().span;
        self.expect(TokenKind::RBrace, "'}'");
        self.file.add_expr(Expr::When { subject, arms }, Span::new(start.lo, end.hi))
    }

    /// A branch/body of `if`/`when`/`for`: a block, or a single statement. A bare expression keeps
    /// its value (exposed as the wrapping block's trailing value); a real statement (`return`,
    /// assignment, `s += i`, …) yields a Unit-valued block.
    fn parse_branch(&mut self) -> ExprId {
        if self.at(TokenKind::LBrace) {
            return self.parse_block_expr();
        }
        let start = self.tok().span;
        let s = self.parse_stmt();
        // A bare expression branch stays a bare expression (its value is the branch value);
        // a real statement (`return`, assignment, `s += i`, …) becomes a Unit-valued block.
        if let Stmt::Expr(e) = self.file.stmt(s) {
            return *e;
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(Expr::Block { stmts: vec![s], trailing: None }, Span::new(start.lo, end.hi))
    }
}

// ---- precedence ----
const BP_PREFIX: u8 = 13;

/// Soft modifiers that don't change a declaration's *kind* (so krusty can ignore them). Excludes
/// `data`/`enum`/`annotation`/`value`/`object`/`companion`/`inner`/`expect`/`actual`,
/// which would alter parsing/semantics and must remain unsupported. `sealed` is included: it maps
/// cleanly onto an abstract, open class (see the top-level dispatch), so ignoring its
/// exhaustiveness aspect never miscompiles.
fn is_modifier(text: &str) -> bool {
    // NOTE: `tailrec`/`external` are deliberately excluded — ignoring them changes semantics
    // (no tail-call optimization → stack overflow; no native body), which would *miscompile*
    // rather than skip. Leaving them unrecognized makes such declarations cleanly unsupported.
    matches!(
        text,
        "public" | "private" | "internal" | "protected" | "open" | "final" | "abstract"
            | "inline" | "noinline" | "crossinline" | "operator" | "override" | "suspend"
            | "lateinit" | "infix" | "reified" | "vararg" | "const" | "sealed"
    )
}

fn compound_op(k: TokenKind) -> Option<BinOp> {
    Some(match k {
        TokenKind::PlusEq => BinOp::Add,
        TokenKind::MinusEq => BinOp::Sub,
        TokenKind::StarEq => BinOp::Mul,
        TokenKind::SlashEq => BinOp::Div,
        TokenKind::PercentEq => BinOp::Rem,
        _ => return None,
    })
}

fn infix_op(k: TokenKind) -> Option<BinOp> {
    Some(match k {
        TokenKind::OrOr => BinOp::Or,
        TokenKind::AndAnd => BinOp::And,
        TokenKind::EqEq => BinOp::Eq,
        TokenKind::NotEq => BinOp::Ne,
        TokenKind::Lt => BinOp::Lt,
        TokenKind::LtEq => BinOp::Le,
        TokenKind::Gt => BinOp::Gt,
        TokenKind::GtEq => BinOp::Ge,
        TokenKind::Plus => BinOp::Add,
        TokenKind::Minus => BinOp::Sub,
        TokenKind::Star => BinOp::Mul,
        TokenKind::Slash => BinOp::Div,
        TokenKind::Percent => BinOp::Rem,
        _ => return None,
    })
}

/// (left binding power, right binding power). Left-assoc => rbp = lbp + 1.
fn infix_bp(op: BinOp) -> (u8, u8) {
    match op {
        BinOp::Or => (1, 2),
        BinOp::And => (3, 4),
        BinOp::Eq | BinOp::Ne => (5, 6),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => (7, 8),
        BinOp::Add | BinOp::Sub => (9, 10),
        BinOp::Mul | BinOp::Div | BinOp::Rem => (11, 12),
    }
}

/// Decode a `'x'` char literal (with simple escapes) to a `char`.
fn unquote_char(raw: &str) -> char {
    let inner = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(raw);
    let mut chars = inner.chars();
    match chars.next() {
        Some('\\') => match chars.next() {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('\\') => '\\',
            Some('\'') => '\'',
            Some('"') => '"',
            Some('0') => '\0',
            Some('$') => '$',
            Some(other) => other,
            None => '\0',
        },
        Some(c) => c,
        None => '\0',
    }
}

/// Unescape a literal chunk of a string template (no surrounding quotes).
fn unescape_chunk(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('$') => out.push('$'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn unquote(raw: &str) -> String {
    let inner = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('$') => out.push('$'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn tree(src: &str) -> String {
        let mut d = DiagSink::new();
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "unexpected parse errors: {}", d.render("test", src));
        file.debug_tree()
    }

    #[test]
    fn simple_fun() {
        assert_eq!(tree("fun add(a: Int, b: Int): Int = a + b"),
            "(fun add (param a Int) (param b Int) :Int (+ a b))\n");
    }

    #[test]
    fn precedence_mul_over_add() {
        assert_eq!(tree("fun f(a: Int, b: Int, c: Int): Int = a + b * c"),
            "(fun f (param a Int) (param b Int) (param c Int) :Int (+ a (* b c)))\n");
    }

    #[test]
    fn precedence_comparison_and_logic() {
        // a < b && c == d  =>  (&& (< a b) (== c d))
        assert_eq!(tree("fun f(a: Int, b: Int, c: Int, d: Int): Boolean = a < b && c == d"),
            "(fun f (param a Int) (param b Int) (param c Int) (param d Int) :Boolean (&& (< a b) (== c d)))\n");
    }

    #[test]
    fn left_assoc_sub() {
        // a - b - c => ((a - b) - c)
        assert_eq!(tree("fun f(a: Int, b: Int, c: Int): Int = a - b - c"),
            "(fun f (param a Int) (param b Int) (param c Int) :Int (- (- a b) c))\n");
    }

    #[test]
    fn paren_overrides() {
        assert_eq!(tree("fun f(a: Int, b: Int, c: Int): Int = (a + b) * c"),
            "(fun f (param a Int) (param b Int) (param c Int) :Int (* (+ a b) c))\n");
    }

    #[test]
    fn member_call() {
        assert_eq!(tree("fun f(a: Int, b: String): String = a.toString() + b"),
            "(fun f (param a Int) (param b String) :String (+ (call (. a toString)) b))\n");
    }

    #[test]
    fn unary_neg() {
        assert_eq!(tree("fun f(a: Int, b: Int): Int = -a * b"),
            "(fun f (param a Int) (param b Int) :Int (* (neg a) b))\n");
    }

    #[test]
    fn if_expr() {
        assert_eq!(tree("fun max(a: Int, b: Int): Int = if (a > b) a else b"),
            "(fun max (param a Int) (param b Int) :Int (if (> a b) a b))\n");
    }

    #[test]
    fn block_body_with_locals_and_while() {
        let t = tree(
            "fun fib(n: Int): Int {\n  var a = 0\n  var b = 1\n  var i = 0\n  while (i < n) {\n    val t = a + b\n    a = b\n    b = t\n    i = i + 1\n  }\n  return a\n}",
        );
        assert!(t.contains("(var a 0)"), "{t}");
        assert!(t.contains("(while (< i n)"), "{t}");
        assert!(t.contains("(set a b)"), "{t}");
        assert!(t.contains("(return a)"), "{t}");
    }

    #[test]
    fn class_with_properties() {
        assert_eq!(tree("class Point(val x: Int, var y: String)"),
            "(class Point (val x Int) (var y String))\n");
    }

    #[test]
    fn class_with_empty_body() {
        assert_eq!(tree("class Box(val v: Int) {\n}"), "(class Box (val v Int))\n");
    }

    #[test]
    fn modifiers_and_annotations_are_skipped() {
        // Leading modifiers + annotations are ignored; the declaration parses normally.
        assert_eq!(tree("public inline fun f(): Int = 1"), "(fun f :Int 1)\n");
        assert_eq!(tree("@JvmStatic fun g(): Int = 2"), "(fun g :Int 2)\n");
        assert_eq!(tree("@Anno(1, 2) open class C(private val x: Int)"),
            "(class C (val x Int))\n");
        // `data` is NOT a skippable modifier — it stays a data class.
        assert_eq!(tree("data class P(val x: Int)"), "(class P (val x Int))\n");
    }

    #[test]
    fn nullable_null_notnull_elvis() {
        assert_eq!(tree("fun f(s: String?): String = s ?: \"d\""),
            "(fun f (param s String) :String (?: s \"d\"))\n");
        assert_eq!(tree("fun g(s: String?): String = s!!"),
            "(fun g (param s String) :String (!! s))\n");
        assert_eq!(tree("fun h(): String = null"),
            "(fun h :String null)\n");
        // chained prefix `!` must NOT be confused with the postfix `!!` operator.
        assert_eq!(tree("fun n(p: Boolean): Boolean = !!!p"),
            "(fun n (param p Boolean) :Boolean (not (not (not p))))\n");
    }

    #[test]
    fn for_loop_and_compound_assign() {
        let t = tree("fun f(n: Int): Int {\n var s = 0\n for (i in 1..n) s += i\n return s\n}");
        assert!(t.contains("(for i (1 .. n)"), "{t}");
        assert!(t.contains("(set s (+ s i))"), "{t}");
        assert!(tree("fun g(n: Int) {\n for (i in n downTo 0 step 2) {}\n}").contains("(for i (n downTo 0 step 2)"));
        assert!(tree("fun h(n: Int) {\n for (i in 0 until n) {}\n}").contains("(for i (0 until n)"));
    }

    #[test]
    fn when_subject_and_subjectless() {
        assert_eq!(
            tree("fun f(n: Int): Int = when (n) { 0 -> 1; 1, 2 -> 2; else -> 9 }"),
            "(fun f (param n Int) :Int (when n (arm 0 => 1) (arm 1 2 => 2) (arm else => 9)))\n"
        );
        assert_eq!(
            tree("fun g(n: Int): Int = when { n < 0 -> 1; else -> 2 }"),
            "(fun g (param n Int) :Int (when (arm (< n 0) => 1) (arm else => 2)))\n"
        );
    }

    #[test]
    fn data_class_parses() {
        // `data` is a soft keyword; the class is otherwise parsed normally.
        assert_eq!(tree("data class Point(val x: Int, val y: Int)"),
            "(class Point (val x Int) (val y Int))\n");
        // `data` remains usable as an ordinary identifier.
        assert_eq!(tree("fun f(data: Int): Int = data"),
            "(fun f (param data Int) :Int data)\n");
    }

    #[test]
    fn class_with_member_function() {
        assert_eq!(
            tree("class Calc(val base: Int) {\n  fun addTo(n: Int): Int = base + n\n}"),
            "(class Calc (val base Int) (method addTo (param n Int) :Int))\n"
        );
    }

    #[test]
    fn package_and_multiple_decls() {
        let src = "package demo\nfun a(): Int = 1\nfun b(): Int = 2\n";
        let mut d = DiagSink::new();
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors());
        assert_eq!(file.package.as_deref(), Some("demo"));
        assert_eq!(file.decls.len(), 2);
    }
}
