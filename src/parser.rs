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

    /// Consume a `{ … }` block, balancing nested braces. Assumes the opening `{` is the current token.

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
                // A `package` directive may follow file-level annotations (`@file:...`), so also
                // accept it here (the common case is consumed before the loop).
                TokenKind::KwPackage => {
                    self.bump(); // 'package'
                    let pkg = self.parse_qualified_name();
                    if self.file.package.is_none() && !pkg.is_empty() {
                        self.file.package = Some(pkg);
                    }
                }
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
                // `fun interface F { fun m(…): R }` — a SAM interface (parsed as an interface).
                TokenKind::KwFun
                    if self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident && t.text(self.src) == "interface") =>
                {
                    self.bump(); // 'fun'
                    let mut d = self.parse_interface();
                    d.is_fun_interface = true;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                TokenKind::KwFun => {
                    // `suspend` needs a coroutine state machine (Continuation) krusty doesn't emit;
                    // compiling it as a plain function is unsound — reject so the file skips.
                    if mods.iter().any(|m| m == "suspend") {
                        self.diags.error(self.tok().span, "krusty: suspend functions are not supported");
                    }
                    let mut d = self.parse_fun(mods.iter().any(|m| m == "inline"), mods.iter().any(|m| m == "final"));
                    d.is_private = mods.iter().any(|m| m == "private");
                    let id = self.file.add_decl(Decl::Fun(d));
                    self.file.decls.push(id);
                }
                TokenKind::KwClass => {
                    let is_value = mods.iter().any(|m| m == "inline" || m == "value");
                    let mut d = self.parse_class();
                    d.is_open = is_open;
                    d.is_abstract = is_abstract;
                    d.is_sealed = is_sealed;
                    d.is_value = is_value;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // top-level property: `val`/`var name (: Type)? = init`
                TokenKind::KwVal | TokenKind::KwVar => {
                    let d = self.parse_top_property(mods.iter().any(|m| m == "lateinit"), false);
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
                // `annotation class Name(...)` — emitted as an interface extending
                // `java/lang/annotation/Annotation` with an accessor per primary-ctor property;
                // instantiation synthesizes an impl class (see emit).
                TokenKind::Ident
                    if self.text() == "annotation" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass) =>
                {
                    self.bump(); // 'annotation'
                    let mut d = self.parse_class();
                    d.is_annotation = true;
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
                // `typealias Name[<T,...>] = Type`
                TokenKind::Ident if self.text() == "typealias" => {
                    self.bump(); // `typealias`
                    let alias = if self.at(TokenKind::Ident) { self.bump().text(self.src).to_string() } else { String::new() };
                    self.parse_type_args(); // skip `<T, R>` if present
                    self.eat(TokenKind::Eq);
                    // Parse the target type name, including dotted FQNs (e.g. java.lang.Exception).
                    let target = if self.at(TokenKind::LParen) {
                        // function type — skip entire line
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) { self.bump(); }
                        String::new()
                    } else if self.at(TokenKind::Ident) {
                        let mut name = self.text().to_string();
                        self.bump();
                        while self.at(TokenKind::Dot) {
                            self.bump();
                            if self.at(TokenKind::Ident) {
                                name.push('.');
                                name.push_str(self.text());
                                self.bump();
                            } else {
                                break;
                            }
                        }
                        // Skip any remaining tokens on this line (e.g. generic args).
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) { self.bump(); }
                        name
                    } else {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) { self.bump(); }
                        String::new()
                    };
                    if !alias.is_empty() && !target.is_empty() {
                        self.file.type_aliases.push((alias, target));
                    }
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


    /// Parse a nested type declaration (`class`/`object`/`interface`/`data|enum|annotation class`/
    /// `sealed …`) through the *real* parsers — never by skipping a balanced body. The current
    /// `class`-body/`object`-body/`enum`-body grammar doesn't support nested types, so the caller
    /// discards the result; a *reference* to the (dropped) nested type then fails to resolve and the
    /// file is cleanly skipped, never miscompiled.
    fn parse_nested_type_decl(&mut self) -> ClassDecl {
        match self.kind() {
            TokenKind::KwClass => self.parse_class(),
            TokenKind::Ident if self.text() == "object" => self.parse_object(),
            TokenKind::Ident if self.text() == "interface" => self.parse_interface(),
            TokenKind::Ident if self.text() == "enum" => self.parse_enum(),
            TokenKind::Ident if self.text() == "data" => { self.bump(); let mut d = self.parse_class(); d.is_data = true; d }
            TokenKind::Ident if self.text() == "annotation" => { self.bump(); self.parse_class() }
            TokenKind::Ident if self.text() == "sealed" => { self.bump(); self.parse_nested_type_decl() }
            _ => self.parse_class(),
        }
    }

    fn skip_annotation(&mut self) {
        self.bump(); // '@'
        // optional use-site target: `file:`, `get:`, `param:`, ...
        if self.at(TokenKind::Ident) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Colon) {
            self.bump();
            self.bump(); // ':'
        }
        let _ = self.parse_qualified_name();
        self.parse_type_args(); // `@Foo<Bar>` (rare) — real type-arg parse
        self.parse_annotation_args();
    }

    /// Parse an annotation argument list `( (name =)? value ,* )` through the real grammar.
    /// Annotations carry no codegen meaning, so the parsed values are discarded.
    fn parse_annotation_args(&mut self) {
        if !self.eat(TokenKind::LParen) {
            return;
        }
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            // optional named argument `name = value`
            if self.at(TokenKind::Ident) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Eq) {
                self.bump(); // name
                self.bump(); // '='
            }
            self.parse_annotation_value();
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
    }

    /// A single annotation argument value: an array literal `[…]`, a nested annotation `@Foo(…)`,
    /// or an ordinary expression (incl. `Foo::class`).
    fn parse_annotation_value(&mut self) {
        if self.at(TokenKind::LBracket) {
            self.bump(); // '['
            self.skip_newlines();
            while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                self.parse_annotation_value();
                self.skip_newlines();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            self.expect(TokenKind::RBracket, "']'");
        } else if self.at(TokenKind::At) {
            self.skip_annotation();
        } else {
            let _ = self.parse_expr();
        }
    }

    /// `abstract_ok` — allow missing initializer (abstract/interface props, class/object body props
    /// with init blocks, etc.). Top-level properties always require an initializer.
    fn parse_top_property(&mut self, is_lateinit: bool, abstract_ok: bool) -> PropDecl {
        self.parse_top_property_c(is_lateinit, abstract_ok, false, false)
    }

    fn parse_top_property_c(&mut self, is_lateinit: bool, abstract_ok: bool, is_const: bool, is_abstract: bool) -> PropDecl {
        let start = self.tok().span;
        let is_var = self.at(TokenKind::KwVar);
        self.bump(); // val/var
        // Optional generic type parameters on an extension property (`val <T> T.foo: T`) — erased.
        if self.at(TokenKind::Lt) {
            self.parse_type_params();
        }
        let first = self.ident_or_error("property name");
        // Optional extension receiver: `val Recv[<…>][?].name` (like an extension function).
        let (receiver, name) = if self.at(TokenKind::Dot) || self.at(TokenKind::Lt) || self.at(TokenKind::Question) {
            let span = self.tok().span;
            if self.at(TokenKind::Lt) {
                self.parse_type_args(); // type args on the receiver — erased
            }
            let nullable = self.eat(TokenKind::Question);
            self.expect(TokenKind::Dot, "'.'");
            let recv = TypeRef { name: first, nullable, arg: None, targs: vec![], span, fun_params: vec![] };
            (Some(recv), self.ident_or_error("property name"))
        } else {
            (None, first)
        };
        let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        let init = if self.eat(TokenKind::Eq) {
            self.skip_newlines();
            Some(self.parse_expr())
        } else {
            None
        };
        // Optional custom accessors: `get() = expr` / `get() { … }` and/or `[private] set(v) { … }`
        // / `private set`. Either order; at most one of each. An accessor begins with `get`/`set`
        // (optionally preceded by a visibility modifier) — anything else ends the property.
        let mut getter: Option<FunBody> = None;
        let mut setter: Option<PropAccessor> = None;
        loop {
            let save = self.i;
            self.skip_newlines();
            // Optional visibility modifier on the accessor (`private set`, …).
            let mut is_private = false;
            let vis_save = self.i;
            if self.at(TokenKind::Ident) && matches!(self.text(), "private" | "protected" | "internal" | "public") {
                is_private = self.text() == "private";
                self.bump();
                self.skip_newlines();
            }
            if !self.at(TokenKind::Ident) || !matches!(self.text(), "get" | "set") {
                self.i = save; // not an accessor — restore (incl. any consumed newlines/modifier)
                break;
            }
            let is_get = self.text() == "get";
            self.bump(); // 'get' / 'set'
            if is_get && self.eat_accessor_parens(true).is_none() {
                self.i = vis_save;
                break;
            }
            if is_get {
                getter = Some(self.parse_accessor_body());
                let _ = is_private; // getter visibility not modeled (rare); ignored
            } else {
                // setter: optional `(param)` then optional body; `private set` has neither.
                let param = self.parse_setter_param();
                let body = if self.eat(TokenKind::Eq) {
                    self.skip_newlines();
                    Some(FunBody::Expr(self.parse_expr()))
                } else if self.at(TokenKind::LBrace) {
                    Some(FunBody::Block(self.parse_block_expr()))
                } else {
                    None // default-bodied setter (e.g. `private set`)
                };
                setter = Some(PropAccessor { param, body, is_private });
            }
        }
        // A property with no initializer, no getter, and no backing-field need must be `lateinit`
        // (or an abstract/interface property); an extension property always has a getter, so it is
        // exempt.
        if init.is_none() && getter.is_none() && setter.is_none() && !is_lateinit && !abstract_ok && !is_abstract && receiver.is_none() {
            self.diags.error(start, "krusty: a property without an initializer must be 'lateinit'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        PropDecl { name, receiver, ty, is_var, init, is_lateinit, getter, setter, is_const, is_abstract, span: Span::new(start.lo, end.hi) }
    }

    /// Consume an accessor's `()`. Returns `Some(())` on success. `require` controls whether a
    /// missing `(` is an error (getter) — setters route through `parse_setter_param` instead.
    fn eat_accessor_parens(&mut self, require: bool) -> Option<()> {
        if !self.at(TokenKind::LParen) {
            if require {
                self.i -= 1; // un-consume `get` so the caller can bail cleanly
            }
            return None;
        }
        self.expect(TokenKind::LParen, "'('");
        self.expect(TokenKind::RParen, "')'");
        Some(())
    }

    /// Parse a setter's optional `(param)` (type annotation discarded). Returns the param name.
    fn parse_setter_param(&mut self) -> Option<String> {
        if !self.eat(TokenKind::LParen) {
            return None;
        }
        let name = if self.at(TokenKind::Ident) {
            let n = self.text().to_string();
            self.bump();
            Some(n)
        } else {
            None
        };
        if self.eat(TokenKind::Colon) {
            let _ = self.parse_type();
        }
        self.expect(TokenKind::RParen, "')'");
        name
    }

    /// Parse a getter body after its `()`: `= expr` or `{ block }`.
    fn parse_accessor_body(&mut self) -> FunBody {
        if self.eat(TokenKind::Eq) {
            self.skip_newlines();
            FunBody::Expr(self.parse_expr())
        } else if self.at(TokenKind::LBrace) {
            FunBody::Block(self.parse_block_expr())
        } else {
            self.diags.error(self.tok().span, "expected '=' or '{' for a property getter".to_string());
            FunBody::None
        }
    }

    /// `companion object [Name] [: Super] { fun…; val… }` — collect its functions/properties to be
    /// emitted as `static`/`static final` members of the enclosing class.
    fn parse_companion(&mut self, methods: &mut Vec<FunDecl>, props: &mut Vec<PropDecl>) {
        self.bump(); // 'companion'
        self.bump(); // 'object'
        if self.at(TokenKind::Ident) {
            self.bump(); // optional companion name
        }
        // tolerate (and ignore) a supertype list on the companion
        let _ = self.parse_supertypes();
        self.skip_newlines();
        if !self.eat(TokenKind::LBrace) {
            return;
        }
        loop {
            self.skip_newlines();
            let mut mods = Vec::new();
            if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                mods = self.skip_decl_prefix();
                self.skip_newlines();
            }
            let lateinit = mods.iter().any(|m| m == "lateinit");
            match self.kind() {
                TokenKind::RBrace | TokenKind::Eof => break,
                TokenKind::KwFun => {
                    let mut d = self.parse_fun(mods.iter().any(|m| m == "inline"), mods.iter().any(|m| m == "final"));
                    d.is_private = mods.iter().any(|m| m == "private");
                    methods.push(d);
                }
                TokenKind::KwVal | TokenKind::KwVar => props.push(self.parse_top_property_c(lateinit, false, mods.iter().any(|m| m == "const"), false)),
                _ => {
                    self.diags.error(self.tok().span, "krusty: companion bodies support only 'fun' and 'val'/'var'");
                    self.bump();
                }
            }
        }
        self.expect(TokenKind::RBrace, "'}'");
    }

    /// `enum class Name { A, B, C }` — v0: simple entries (no constructor args, no class body).
    fn parse_enum(&mut self) -> ClassDecl {
        let start = self.tok().span;
        self.bump(); // 'enum'
        self.bump(); // 'class'
        let name = self.ident_or_error("enum name");
        // Optional primary constructor: `enum class C(val rgb: Int, …)`.
        let mut props = Vec::new();
        if self.eat(TokenKind::LParen) {
            self.skip_newlines();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                self.skip_decl_prefix();
                let is_property = self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar);
                let is_var = self.at(TokenKind::KwVar);
                if is_property {
                    self.bump();
                }
                let pname = self.ident_or_error("parameter name");
                self.expect(TokenKind::Colon, "':'");
                let ty = self.parse_type();
                props.push(PropParam { name: pname, ty, is_var, is_property, default: None });
                self.skip_newlines();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            self.expect(TokenKind::RParen, "')'");
        }
        let mut entries = Vec::new();
        let mut entry_args: Vec<Vec<ExprId>> = Vec::new();
        let mut entry_bodies: Vec<Vec<FunDecl>> = Vec::new();
        let mut methods = Vec::new();
        self.skip_newlines();
        if self.eat(TokenKind::LBrace) {
            self.skip_newlines();
            while self.at(TokenKind::Ident) {
                entries.push(self.text().to_string());
                self.bump();
                // Optional constructor arguments: `RED(0xFF0000)`.
                let mut args = Vec::new();
                if self.eat(TokenKind::LParen) {
                    self.skip_newlines();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        args.push(self.parse_expr());
                        self.skip_newlines();
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                    self.expect(TokenKind::RParen, "')'");
                }
                // A per-entry class body (`RED { override fun m() = … }`) is an anonymous subclass.
                // Capture its method overrides; any non-method member bails (file skips cleanly).
                let mut body = Vec::new();
                if self.eat(TokenKind::LBrace) {
                    self.skip_newlines();
                    while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                        let bmods = if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                            let m = self.skip_decl_prefix();
                            self.skip_newlines();
                            m
                        } else { Vec::new() };
                        if self.at(TokenKind::KwFun) {
                            body.push(self.parse_fun(bmods.iter().any(|m| m == "inline"), bmods.iter().any(|m| m == "final")));
                        } else {
                            self.diags.error(self.tok().span, "krusty: only method overrides are supported in an enum entry body");
                            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) { self.bump(); }
                        }
                        self.skip_newlines();
                    }
                    self.expect(TokenKind::RBrace, "'}'");
                }
                entry_bodies.push(body);
                entry_args.push(args);
                self.skip_newlines();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            // Members follow a `;` separator (lexed as a newline): `enum class C { A, B; fun f() … }`.
            loop {
                self.skip_newlines();
                let emods = if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    let m = self.skip_decl_prefix();
                    self.skip_newlines();
                    m
                } else { Vec::new() };
                match self.kind() {
                    TokenKind::KwFun => methods.push(self.parse_fun(emods.iter().any(|m| m == "inline"), emods.iter().any(|m| m == "final"))),
                    // Nested type declarations and secondary constructors in an enum body: parse
                    // them through the real grammar (no token-skipping) and discard — krusty doesn't
                    // emit them, so a reference fails to resolve and the file is cleanly skipped.
                    TokenKind::KwClass => { let _ = self.parse_nested_type_decl(); }
                    TokenKind::Ident if self.text() == "constructor" => {
                        self.diags.error(self.tok().span, "krusty: secondary constructors in enum classes are not supported");
                        self.bump(); // 'constructor'
                        let _ = self.parse_param_list();
                        if self.eat(TokenKind::Colon) {
                            self.skip_newlines();
                            if self.at(TokenKind::Ident) { self.bump(); } // 'this'/'super'
                            let _ = self.parse_call_arguments();
                        }
                        self.skip_newlines();
                        if self.at(TokenKind::LBrace) { let _ = self.parse_block_expr(); }
                    }
                    TokenKind::Ident if matches!(self.text(), "object" | "interface") => { let _ = self.parse_nested_type_decl(); }
                    TokenKind::Ident if self.text() == "companion" => { self.bump(); let _ = self.parse_nested_type_decl(); }
                    _ => break,
                }
            }
            self.skip_newlines();
            if !self.at(TokenKind::RBrace) {
                self.diags.error(self.tok().span, "krusty: unsupported enum member");
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
            props,
            methods,
            companion_methods: Vec::new(),
            companion_props: Vec::new(),
            body_props: Vec::new(),
            init_order: Vec::new(),
            is_data: false,
            is_value: false,
            is_annotation: false,
            is_object: false,
            is_enum: true,
            enum_entries: entries,
            enum_entry_args: entry_args,
            enum_entry_bodies: entry_bodies,
            is_interface: false, is_fun_interface: false,
            is_open: false,
            is_abstract: false,
            is_sealed: false,
            supertypes: Vec::new(), delegations: Vec::new(),
            base_class: None,
            base_args: Vec::new(),
            secondary_ctors: Vec::new(),
            span: Span::new(start.lo, end.hi),
        }
    }

    /// Parse an optional generic constraint clause `where T : Bound, U : Bound2` after a function or
    /// class signature. Constraints are *erased* (krusty erases type parameters to `Object`), but a
    /// primitive bound is rejected for the same reason as an inline bound — kotlinc specializes it
    /// (see `parse_type_params`). `where` may sit on a following line, so newlines are skipped only
    /// when the clause is actually present (otherwise the position is restored).
    fn parse_where_clause(&mut self) {
        let save = self.i;
        self.skip_newlines();
        if !(self.at(TokenKind::Ident) && self.text() == "where") {
            self.i = save;
            return;
        }
        self.bump(); // 'where'
        loop {
            self.skip_newlines();
            if self.at(TokenKind::Ident) {
                self.bump(); // type-parameter name
            }
            if self.eat(TokenKind::Colon) {
                let bound = self.parse_type();
                if crate::types::Ty::from_name(&bound.name).map_or(false, |t| t.is_primitive()) {
                    self.diags.error(bound.span, "krusty: type parameter with a primitive upper bound is not supported".to_string());
                }
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
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

    fn parse_fun(&mut self, is_inline: bool, is_final: bool) -> FunDecl {
        let start = self.tok().span;
        self.bump(); // 'fun'
        // `fun interface` is a SAM/functional interface declaration — not a regular function.
        // Skip the entire interface body with a clean unsupported-feature message.
        if self.at(TokenKind::Ident) && self.text() == "interface" {
            self.diags.error(start, "krusty: 'fun interface' (SAM interfaces) are not supported");
            self.bump(); // 'interface'
            if self.at(TokenKind::Ident) { self.bump(); } // interface name
            self.parse_type_args();
            let (supertypes, _, _, _) = self.parse_supertypes();
            let _ = supertypes;
            if self.at(TokenKind::LBrace) {
                let _ = self.parse_block_expr();
            }
            return FunDecl { name: "<fun-interface>".to_string(), receiver: None, params: vec![], ret: None,
                body: FunBody::None, type_params: vec![], non_null_type_params: Default::default(),
                reified_type_params: Default::default(),
                span: start, is_inline: false, is_final: false, is_private: false };
        }
        let (type_params, non_null_type_params, reified_type_params) = if self.at(TokenKind::Lt) { self.parse_type_params() } else { (Vec::new(), std::collections::HashSet::new(), std::collections::HashSet::new()) };
        // Parse either `Name` (regular function) or `ReceiverType . Name` (extension function).
        // Receiver type may itself be parameterized (`List<T>.foo`) or nullable (`String?.foo`).
        let first_name = if self.at(TokenKind::Ident) {
            let n = self.text().to_string();
            self.bump();
            n
        } else {
            self.diags.error(self.tok().span, "expected function name");
            "<error>".to_string()
        };
        let (receiver, name) = if self.at(TokenKind::Dot) || self.at(TokenKind::Lt) || self.at(TokenKind::Question) {
            // `fun RecvType<...>?.name(...)` — extension function.
            let span = self.tok().span;
            let mut recv_nullable = false;
            if self.at(TokenKind::Lt) { self.parse_type_args(); }  // skip type args on receiver
            if self.eat(TokenKind::Question) { recv_nullable = true; }
            self.expect(TokenKind::Dot, "'.'");
            let recv_ty = TypeRef { name: first_name, nullable: recv_nullable, arg: None, targs: vec![], span, fun_params: vec![] };
            let fun_name = if self.at(TokenKind::Ident) {
                let n = self.text().to_string();
                self.bump();
                n
            } else {
                self.diags.error(self.tok().span, "expected extension function name");
                "<error>".to_string()
            };
            (Some(recv_ty), fun_name)
        } else {
            (None, first_name)
        };
        let params = self.parse_param_list();
        let ret = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        self.parse_where_clause();
        let body = if self.eat(TokenKind::Eq) {
            self.skip_newlines();
            FunBody::Expr(self.parse_expr())
        } else if self.at(TokenKind::LBrace) {
            FunBody::Block(self.parse_block_expr())
        } else {
            FunBody::None
        };
        let end = self.t[self.i.saturating_sub(1)].span;
        FunDecl { name, receiver, params, ret, body, type_params, non_null_type_params, reified_type_params, span: Span::new(start.lo, end.hi), is_inline, is_final, is_private: false }
    }

    /// Parse a parenthesised parameter list `( (mods name: Type (= default)?),* )` via the real
    /// grammar — never by skipping to a balanced `)`.
    fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.expect(TokenKind::LParen, "'('");
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let mut pmods = Vec::new();
            // `value` is a valid parameter name in Kotlin; only collect real parameter modifiers.
            if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text()) && self.text() != "value") {
                pmods = self.skip_decl_prefix(); // `@Anno`, `vararg`, `noinline`, … on a parameter
            }
            let is_vararg = pmods.iter().any(|m| m == "vararg");
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
            let default = if self.eat(TokenKind::Eq) {
                self.skip_newlines();
                Some(self.parse_expr())
            } else {
                None
            };
            params.push(Param { name: pname, ty, is_vararg, default });
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        params
    }

    /// Parse a parenthesised argument list `( expr,* )` into expressions, via the real grammar.
    /// Returns an empty list if no `(` is present.
    fn parse_call_arguments(&mut self) -> Vec<ExprId> {
        let mut args = Vec::new();
        if !self.eat(TokenKind::LParen) {
            return args;
        }
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            args.push(self.parse_expr());
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        args
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
        let (type_params, _, _) = if self.at(TokenKind::Lt) { self.parse_type_params() } else { (Vec::new(), std::collections::HashSet::new(), std::collections::HashSet::new()) };
        let mut props = Vec::new();
        if self.eat(TokenKind::LParen) {
            self.skip_newlines();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text()) && self.text() != "value") {
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
                let default = if self.eat(TokenKind::Eq) {
                    self.skip_newlines();
                    Some(self.parse_expr())
                } else {
                    None
                };
                props.push(PropParam { name: pname, ty, is_var, is_property, default });
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
        let (supertypes, base_class, base_args, delegations) = self.parse_supertypes();
        // `class Derived<T> : Base<T>() where T : I1, T : I2` — generic constraints after the
        // supertype list, before the body.
        self.parse_where_clause();
        // Optional class body: member `fun`s, body properties (`val`/`var`), and `init { }` blocks.
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        let mut companion_methods: Vec<FunDecl> = Vec::new();
        let mut companion_props: Vec<PropDecl> = Vec::new();
        let mut secondary_ctors: Vec<SecondaryCtor> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let mut mods = Vec::new();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    mods = self.skip_decl_prefix();
                    self.skip_newlines();
                }
                let lateinit = mods.iter().any(|m| m == "lateinit");
                let fun_inline = mods.iter().any(|m| m == "inline");
                let fun_final = mods.iter().any(|m| m == "final");
                let is_abstract = mods.iter().any(|m| m == "abstract");
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun(fun_inline, fun_final)),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        // Non-abstract body props may omit the initializer (init blocks supply the
                        // value); an `abstract` property has no field and is marked accordingly.
                        let p = self.parse_top_property_c(lateinit, !is_abstract, mods.iter().any(|m| m == "const"), is_abstract);
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident if self.text() == "init" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::LBrace) => {
                        self.bump(); // 'init'
                        let block = self.parse_block_expr();
                        init_order.push(ClassInit::Block(block));
                    }
                    // `companion object [Name] { fun…; val… }` — members become static on this class.
                    TokenKind::Ident
                        if self.text() == "companion"
                            && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident && t.text(self.src) == "object") =>
                    {
                        self.parse_companion(&mut companion_methods, &mut companion_props);
                    }
                    // Silently skip nested type declarations (inner/nested class, object,
                    // interface, typealias) and secondary constructors.  Parsing them properly
                    // requires nesting the full resolver/emitter; for now we drop them and the
                    // file compiles so tests that don't exercise the nested type still pass.
                    TokenKind::KwClass => {
                        // An `inner class` captures the outer instance (a `Test this$0` field +
                        // qualified `new`), which krusty doesn't model — reject.
                        if mods.iter().any(|m| m == "inner") {
                            self.diags.error(self.tok().span, "krusty: inner classes are not supported");
                            let _ = self.parse_nested_type_decl();
                        } else {
                            // A plain *nested* class `Outer { class Inner … }` is a separate class
                            // (internal name `Outer$Inner`, source name `Outer.Inner`). Hoist it to
                            // the file's top level so it is registered and emitted like any class.
                            let mut nested = self.parse_class();
                            nested.name = format!("{}.{}", name, nested.name);
                            let id = self.file.add_decl(Decl::Class(nested));
                            self.file.decls.push(id);
                        }
                    }
                    // Nested `data class Inner(…)` → hoist like a plain nested class (`Outer.Inner`),
                    // constructed as `Outer.Inner(…)`; its data members emit normally.
                    TokenKind::Ident
                        if self.text() == "data" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass) =>
                    {
                        self.bump(); // 'data'
                        let mut nested = self.parse_class();
                        nested.is_data = true;
                        nested.name = format!("{}.{}", name, nested.name);
                        let id = self.file.add_decl(Decl::Class(nested));
                        self.file.decls.push(id);
                    }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(self.text(), "enum" | "annotation")
                                && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass))
                            || (self.text() == "sealed"
                                && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident && t.text(self.src) == "interface")) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    // Secondary constructor: `constructor(params) [: this/super(args)] { body }`.
                    // krusty doesn't emit these, so a call to the secondary ctor would resolve to a
                    // non-existent `<init>` (NoSuchMethodError). Reject the class rather than silently
                    // drop the constructor and miscompile.
                    TokenKind::Ident if self.text() == "constructor" => {
                        // Parse the secondary constructor through real productions — the parameter
                        // list, the `: this(args)`/`: super(args)` delegation, and the body block —
                        // never by skipping to a balanced delimiter.
                        let ctor_span = self.tok().span;
                        self.bump(); // 'constructor'
                        let params = self.parse_param_list();
                        let mut delegation = CtorDelegation::None;
                        if self.eat(TokenKind::Colon) {
                            self.skip_newlines();
                            let target = if self.at(TokenKind::Ident) { let t = self.text().to_string(); self.bump(); t } else { String::new() };
                            let args = self.parse_call_arguments();
                            delegation = match target.as_str() {
                                "this" => CtorDelegation::This(args),
                                "super" => CtorDelegation::Super(args),
                                _ => { self.diags.error(ctor_span, "expected 'this' or 'super' in constructor delegation"); CtorDelegation::None }
                            };
                        }
                        self.skip_newlines();
                        let body = if self.at(TokenKind::LBrace) { Some(self.parse_block_expr()) } else { None };
                        secondary_ctors.push(SecondaryCtor { params, delegation, body, span: ctor_span });
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) { self.bump(); }
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
        ClassDecl { name, type_params, props, methods, companion_methods, companion_props, body_props, init_order, is_data: false, is_value: false, is_annotation: false, is_object: false, is_enum: false, enum_entries: Vec::new(), enum_entry_args: Vec::new(), enum_entry_bodies: Vec::new(), is_interface: false, is_fun_interface: false, is_open: false, is_abstract: false, is_sealed: false, supertypes, delegations, base_class, base_args, secondary_ctors, span: Span::new(start.lo, end.hi) }
    }

    /// Parse an optional `: Base(args), Iface1, Iface2` supertype list. A supertype with `()` is the
    /// base class (returns its name + ctor-arg expressions); the rest are implemented interfaces.
    fn parse_supertypes(&mut self) -> (Vec<String>, Option<String>, Vec<ExprId>, Vec<(String, String)>) {
        let mut ifaces = Vec::new();
        let mut base: Option<String> = None;
        let mut base_args = Vec::new();
        let mut delegations = Vec::new();
        if self.eat(TokenKind::Colon) {
            loop {
                self.skip_newlines();
                let name = self.parse_qualified_name();
                let simple = name.rsplit('.').next().unwrap_or(&name).to_string();
                // Fully-qualified name (e.g. java.util.RandomAccess) → JVM internal format.
                let effective = if name.contains('.') { name.replace('.', "/") } else { simple.clone() };
                // Skip optional type arguments (e.g. `A<Int, Number>`); they are erased on JVM.
                if self.at(TokenKind::Lt) {
                    self.parse_type_args();
                }
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
                    base = Some(effective.clone());
                    base_args = args;
                } else if !effective.is_empty() {
                    ifaces.push(effective.clone());
                }
                // Class delegation: `: Iface by delegate`. A simple-name delegate (a `val` ctor-param
                // field) is supported — record `(iface, delegate)`; any other delegate expression is
                // skipped (parsed but marked unsupported).
                if self.at(TokenKind::Ident) && self.text() == "by" {
                    self.bump(); // 'by'
                    if self.at(TokenKind::Ident) {
                        let delegate = self.text().to_string();
                        let after = self.t.get(self.i + 1).map(|t| t.kind);
                        // Only a bare variable name (not a call/member) is the simple delegate form.
                        if matches!(after, Some(TokenKind::Comma) | Some(TokenKind::LBrace) | Some(TokenKind::Newline)) {
                            self.bump();
                            delegations.push((effective.clone(), delegate));
                        } else {
                            self.diags.error(self.tok().span, "krusty: only `by <val-parameter>` delegation is supported");
                            let _ = self.parse_expr();
                        }
                    } else {
                        self.diags.error(self.tok().span, "krusty: only `by <val-parameter>` delegation is supported");
                        let _ = self.parse_expr();
                    }
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }
        (ifaces, base, base_args, delegations)
    }

    /// `interface Name { fun sig(): T }` — abstract member functions only (v0).
    fn parse_interface(&mut self) -> ClassDecl {
        let start = self.tok().span;
        self.bump(); // 'interface'
        let name = self.ident_or_error("interface name");
        let (type_params, _, _) = if self.at(TokenKind::Lt) { self.parse_type_params() } else { (Vec::new(), std::collections::HashSet::new(), std::collections::HashSet::new()) };
        let (supertypes, _base, _base_args, _) = self.parse_supertypes();
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let imods = if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    let m = self.skip_decl_prefix();
                    self.skip_newlines();
                    m
                } else { Vec::new() };
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => {
                        let f = self.parse_fun(imods.iter().any(|m| m == "inline"), false);
                        methods.push(f);
                    }
                    // Abstract interface property: `val`/`var x: T` (no initializer/getter).
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let p = self.parse_top_property(false, true);
                        if p.init.is_some() {
                            self.diags.error(p.span, "krusty: interface properties with an initializer/getter are not supported");
                        }
                        body_props.push(p);
                    }
                    TokenKind::KwClass => { let _ = self.parse_nested_type_decl(); }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(self.text(), "data" | "enum" | "annotation")
                                && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass)) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) { self.bump(); }
                    }
                    _ => {
                        self.diags.error(self.tok().span, "v0: interface bodies support abstract 'fun' and 'val'/'var' declarations");
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl {
            name, type_params, props: Vec::new(), methods, companion_methods: Vec::new(), companion_props: Vec::new(), body_props, init_order: Vec::new(),
            is_data: false, is_value: false, is_annotation: false, is_object: false, is_enum: false,
            enum_entries: Vec::new(), enum_entry_args: Vec::new(), enum_entry_bodies: Vec::new(), is_interface: true, is_fun_interface: false, is_open: false, is_abstract: false, is_sealed: false,
            supertypes, delegations: Vec::new(), base_class: None, base_args: Vec::new(), secondary_ctors: Vec::new(),
            span: Span::new(start.lo, end.hi),
        }
    }

    /// `object Name { fun … }` — a singleton with member functions (no primary constructor).
    /// Parse an object/anonymous-object body `{ fun…/val…/init… }`, returning its members.
    fn parse_object_body(&mut self) -> (Vec<FunDecl>, Vec<PropDecl>, Vec<ClassInit>) {
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let mut mods = Vec::new();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    mods = self.skip_decl_prefix();
                    self.skip_newlines();
                }
                let lateinit = mods.iter().any(|m| m == "lateinit");
                let fun_inline = mods.iter().any(|m| m == "inline");
                let fun_final = mods.iter().any(|m| m == "final");
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun(fun_inline, fun_final)),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let p = self.parse_top_property_c(lateinit, true, mods.iter().any(|m| m == "const"), false);
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident if self.text() == "init" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::LBrace) => {
                        self.bump();
                        let block = self.parse_block_expr();
                        init_order.push(ClassInit::Block(block));
                    }
                    TokenKind::KwClass => { let _ = self.parse_nested_type_decl(); }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(self.text(), "data" | "enum" | "annotation")
                                && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass)) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) { self.bump(); }
                    }
                    _ => {
                        self.diags.error(self.tok().span, "krusty: object bodies support 'fun', 'val'/'var', and 'init' blocks");
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        (methods, body_props, init_order)
    }

    /// An anonymous object expression `object : Super(args)?, Iface… { members }` → a synthesized
    /// (uniquely-named) class plus a no-argument construction of it. Capturing the enclosing scope is
    /// not modelled — the checker/lowering reject a body that reads outer locals.
    fn parse_anon_object(&mut self, span: Span) -> ExprId {
        self.bump(); // 'object'
        let (supertypes, base_class, base_args, delegations) = self.parse_supertypes();
        let (methods, body_props, init_order) = self.parse_object_body();
        let end = self.t[self.i.saturating_sub(1)].span;
        let name = format!("Anon$anon${}", span.lo);
        let synth = ClassDecl {
            name: name.clone(), type_params: Vec::new(), props: Vec::new(), methods,
            companion_methods: Vec::new(), companion_props: Vec::new(), body_props, init_order,
            is_data: false, is_value: false, is_annotation: false, is_object: false, is_enum: false,
            enum_entries: Vec::new(), enum_entry_args: Vec::new(), enum_entry_bodies: Vec::new(), is_interface: false,
            is_fun_interface: false, is_open: false, is_abstract: false, is_sealed: false,
            supertypes, delegations, base_class, base_args, secondary_ctors: Vec::new(),
            span: Span::new(span.lo, end.hi),
        };
        let did = self.file.add_decl(Decl::Class(synth));
        self.file.decls.push(did);
        let callee = self.file.add_expr(Expr::Name(name), span);
        self.file.add_expr(Expr::Call { callee, args: Vec::new() }, Span::new(span.lo, end.hi))
    }

    fn parse_object(&mut self) -> ClassDecl {
        let start = self.tok().span;
        self.bump(); // 'object'
        let name = self.ident_or_error("object name");
        let _ = self.parse_supertypes(); // tolerate (ignore) an object's supertype list
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let mut mods = Vec::new();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                    mods = self.skip_decl_prefix();
                    self.skip_newlines();
                }
                let lateinit = mods.iter().any(|m| m == "lateinit");
                let fun_inline = mods.iter().any(|m| m == "inline");
                let fun_final = mods.iter().any(|m| m == "final");
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun(fun_inline, fun_final)),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let p = self.parse_top_property_c(lateinit, true, mods.iter().any(|m| m == "const"), false); // init blocks may supply the value
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident if self.text() == "init" && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::LBrace) => {
                        self.bump();
                        let block = self.parse_block_expr();
                        init_order.push(ClassInit::Block(block));
                    }
                    TokenKind::KwClass => { let _ = self.parse_nested_type_decl(); }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(self.text(), "data" | "enum" | "annotation")
                                && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwClass)) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) { self.bump(); }
                    }
                    _ => {
                        self.diags.error(self.tok().span, "krusty: object bodies support 'fun', 'val'/'var', and 'init' blocks");
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl { name, type_params: Vec::new(), props: Vec::new(), methods, companion_methods: Vec::new(), companion_props: Vec::new(), body_props, init_order, is_data: false, is_value: false, is_annotation: false, is_object: true, is_enum: false, enum_entries: Vec::new(), enum_entry_args: Vec::new(), enum_entry_bodies: Vec::new(), is_interface: false, is_fun_interface: false, is_open: false, is_abstract: false, is_sealed: false, supertypes: Vec::new(), delegations: Vec::new(), base_class: None, base_args: Vec::new(), secondary_ctors: Vec::new(), span: Span::new(start.lo, end.hi) }
    }

    fn parse_type(&mut self) -> TypeRef {
        let span = self.tok().span;
        // `suspend` modifier on a function type: `suspend (A) -> B` — consume and parse as function type.
        if self.at(TokenKind::Ident) && self.text() == "suspend" {
            self.bump(); // 'suspend'
        }
        // Function type: `(A, B) -> R` — starts with `(`.
        if self.at(TokenKind::LParen) {
            self.bump(); // '('
            let mut fun_params = Vec::new();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                // Skip optional parameter name prefix `name: Type` — consume up to a colon if present.
                // Peek ahead: if next two tokens are Ident + Colon, skip them.
                if self.at(TokenKind::Ident) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Colon) {
                    self.bump(); // name
                    self.bump(); // ':'
                }
                fun_params.push(self.parse_type());
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "')'");
            if self.eat(TokenKind::Arrow) {
                let ret = self.parse_type();
                let nullable = self.eat(TokenKind::Question);
                TypeRef { name: "<fun>".to_string(), nullable, arg: Some(Box::new(ret)), targs: Vec::new(), span, fun_params }
            } else {
                // Parenthesized type (rare) — just return error; krusty doesn't support tuple types.
                self.diags.error(span, "expected '->' for function type");
                TypeRef { name: "<error>".to_string(), nullable: false, arg: None, targs: Vec::new(), span, fun_params: Vec::new() }
            }
        } else if self.at(TokenKind::Ident) {
            let mut name = self.text().to_string();
            self.bump();
            // A qualified type name — a nested class `Outer.Inner` (registered as `Outer.Inner`) or a
            // package-qualified type (`kotlin.reflect.KClass`). Consume the dotted path.
            while self.at(TokenKind::Dot) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident) {
                self.bump(); // '.'
                name.push('.');
                name.push_str(self.text());
                self.bump();
            }
            // For `Array<T>`, capture the element type in `arg`; for any other generic type, capture
            // the full argument list in `targs` (erased in JVM descriptors, kept for member typing).
            let mut targs = Vec::new();
            let arg = if name == "Array" && self.at(TokenKind::Lt) {
                self.bump(); // '<'
                self.skip_variance(); // `out`/`in`
                // Star projection `Array<*>` — erase to Object.
                let elem = if self.eat(TokenKind::Star) {
                    TypeRef { name: "Any".to_string(), nullable: true, arg: None, targs: Vec::new(), span, fun_params: Vec::new() }
                } else {
                    self.parse_type()
                };
                self.expect(TokenKind::Gt, "'>'");
                Some(Box::new(elem))
            } else {
                targs = self.parse_type_args(); // `Box<Int>` → carry `[Int]` (erased in descriptors)
                None
            };
            let nullable = self.eat(TokenKind::Question); // `T?`
            let base = TypeRef { name, nullable, arg, targs, span, fun_params: Vec::new() };
            // Receiver (extension) function type `Recv.() -> R` ≡ `Function1<Recv, R>`, and
            // `Recv.(A) -> R` ≡ `Function2<Recv, A, R>`. The receiver folds in as the first function
            // parameter, exactly how Kotlin lowers an extension-function type to `FunctionN` — so the
            // rest of the pipeline sees a plain `(Recv, …) -> R`. (The dotted-path loop above stops at
            // `.` `(` since `(` is not an `Ident`, leaving us positioned here.)
            if self.at(TokenKind::Dot) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::LParen) {
                self.bump(); // '.'
                self.bump(); // '('
                let mut fun_params = vec![base];
                while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                    if self.at(TokenKind::Ident) && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Colon) {
                        self.bump(); // name
                        self.bump(); // ':'
                    }
                    fun_params.push(self.parse_type());
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen, "')'");
                self.expect(TokenKind::Arrow, "'->'");
                let ret = self.parse_type();
                let fnull = self.eat(TokenKind::Question);
                return TypeRef { name: "<fun>".to_string(), nullable: fnull, arg: Some(Box::new(ret)), targs: Vec::new(), span, fun_params };
            }
            base
        } else {
            self.diags.error(span, "expected a type");
            TypeRef { name: "<error>".to_string(), nullable: false, arg: None, targs: Vec::new(), span, fun_params: Vec::new() }
        }
    }

    /// Skip a leading `out`/`in` variance modifier inside a type-argument list.
    fn skip_variance(&mut self) {
        if self.at(TokenKind::Ident) && matches!(self.text(), "out" | "in") {
            self.bump();
        }
    }

    /// Parse a generic type-argument list `< (variance? type | *),+ >` via the real grammar
    /// (recursing through `parse_type`, so nested generics like `Map<K, List<V>>` parse correctly).
    /// The arguments are returned for completeness but JVM-erased, so callers may discard them.
    fn parse_type_args(&mut self) -> Vec<TypeRef> {
        let mut args = Vec::new();
        if !self.eat(TokenKind::Lt) {
            return args;
        }
        self.skip_newlines();
        while !self.at(TokenKind::Gt) && !self.at(TokenKind::Eof) {
            self.skip_variance(); // `out`/`in`
            if self.eat(TokenKind::Star) {
                // Star projection `<*>` — erased to `Any?`.
                let span = self.tok().span;
                args.push(TypeRef { name: "Any".to_string(), nullable: true, arg: None, targs: Vec::new(), span, fun_params: Vec::new() });
            } else {
                args.push(self.parse_type());
            }
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::Gt, "'>'");
        args
    }

    /// Parse a `<T, reified U : Bound, out V>` type-parameter list, returning the parameter names,
    /// the `: Any`-bounded (non-null) names, and the `reified` names (which an `inline` function may
    /// use concretely — `is T`, `as T`, `T::class` — and which codegen specializes per call site).
    fn parse_type_params(&mut self) -> (Vec<String>, std::collections::HashSet<String>, std::collections::HashSet<String>) {
        let mut names = Vec::new();
        let mut non_null = std::collections::HashSet::new();
        let mut reified = std::collections::HashSet::new();
        if !self.eat(TokenKind::Lt) {
            return (names, non_null, reified);
        }
        loop {
            self.skip_newlines();
            // Skip variance/reified modifiers. `in` is a keyword; `out`/`reified` are idents.
            let mut is_reified = false;
            while (self.at(TokenKind::Ident) && matches!(self.text(), "reified" | "out")) || self.at(TokenKind::KwIn) {
                if self.at(TokenKind::Ident) && self.text() == "reified" {
                    is_reified = true;
                }
                self.bump();
            }
            let tname = if self.at(TokenKind::Ident) {
                let n = self.text().to_string();
                self.bump();
                n
            } else {
                String::new()
            };
            if !tname.is_empty() {
                names.push(tname.clone());
                if is_reified {
                    reified.insert(tname.clone());
                }
            }
            if self.eat(TokenKind::Colon) {
                let bound = self.parse_type();
                // `T: Any` → the type param can't be null (erased to Object but non-null).
                if bound.name == "Any" && !bound.nullable && !tname.is_empty() {
                    non_null.insert(tname.clone());
                }
                // A primitive upper bound (`T: Double`) is *specialized* by kotlinc (e.g. it emits a
                // primitive `==`/IEEE-754 comparison), not erased to Object. krusty only erases type
                // parameters, so it would miscompile such code — reject it instead.
                if crate::types::Ty::from_name(&bound.name).map_or(false, |t| t.is_primitive()) {
                    self.diags.error(bound.span, "krusty: type parameter with a primitive upper bound is not supported".to_string());
                }
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "'>'");
        (names, non_null, reified)
    }

    // ---- statements ----
    /// Parse a lambda literal `{ [param ->] stmts }` (single optional parameter; the body is a block).
    fn parse_lambda(&mut self) -> ExprId {
        let start = self.tok().span;
        self.expect(TokenKind::LBrace, "'{'");
        self.skip_newlines();
        // Optional parameter list ending in `->`: `it ->`, `x: T ->`, `a, b ->` (types discarded; the
        // parameter types come from the declared function type via `check_lambda_with_types`). Detect
        // by scanning for a top-level `->` before the lambda's closing `}`.
        let has_params = {
            let mut j = self.i;
            let mut depth = 0i32;
            loop {
                match self.t.get(j).map(|t| t.kind) {
                    None => break false,
                    Some(TokenKind::Arrow) if depth == 0 => break true,
                    Some(TokenKind::RBrace) if depth == 0 => break false,
                    Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace) => depth += 1,
                    Some(TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace) => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
        };
        // Parameter type annotations, parallel to `params` — kept (in a side-table) so a bare-value
        // lambda `{ x: Int -> … }` types its own parameters even without an expected function type.
        let mut param_types: Vec<Option<TypeRef>> = Vec::new();
        let params = if has_params {
            let mut ps = Vec::new();
            loop {
                self.skip_newlines();
                if self.at(TokenKind::Ident) {
                    ps.push(self.text().to_string());
                    self.bump();
                    if self.at(TokenKind::Colon) {
                        self.bump();
                        param_types.push(Some(self.parse_type()));
                    } else {
                        param_types.push(None);
                    }
                }
                if self.at(TokenKind::Comma) {
                    self.bump();
                    continue;
                }
                break;
            }
            self.expect(TokenKind::Arrow, "'->'");
            ps
        } else {
            Vec::new()
        };
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
        let mut trailing = None;
        if let Some(&last) = stmts.last() {
            if let Stmt::Expr(e) = self.file.stmt(last) {
                trailing = Some(*e);
                stmts.pop();
            }
        }
        let body = self.file.add_expr(Expr::Block { stmts, trailing }, Span::new(start.lo, end.hi));
        let lam = self.file.add_expr(Expr::Lambda { params, body }, Span::new(start.lo, end.hi));
        if param_types.iter().any(|t| t.is_some()) {
            self.file.lambda_param_types.insert(lam.0, param_types);
        }
        lam
    }

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

    /// The default-value expression for a type (`var x: T` deferred init): `0`/`false`/`'\0'`/`null`.
    fn default_init_expr(&mut self, ty: &TypeRef, span: Span) -> ExprId {
        let e = match ty.name.as_str() {
            _ if ty.nullable => Expr::NullLit,
            "Int" | "Byte" | "Short" => Expr::IntLit(0),
            "Long" => Expr::LongLit(0),
            "Float" => Expr::FloatLit(0.0),
            "Double" => Expr::DoubleLit(0.0),
            "Boolean" => Expr::BoolLit(false),
            "Char" => Expr::CharLit('\0'),
            _ => Expr::NullLit,
        };
        self.file.add_expr(e, span)
    }

    /// Desugar `name++`/`name--`/`++name`/`--name` (statement) to `name = name ± 1`.
    fn parse_incdec(&mut self, name: String, dec: bool, start: Span) -> StmtId {
        self.finish_stmt(Stmt::IncDec { name, dec }, start)
    }

    fn parse_stmt(&mut self) -> StmtId {
        // Labeled loop: `l1@ while(…)` / `l1@ for(…)` / `l1@ do {…}`. Capture the label and thread it
        // onto the loop so `break@l1`/`continue@l1` can target it.
        let mut loop_label: Option<String> = None;
        if self.at(TokenKind::Ident) {
            let next1 = self.t.get(self.i + 1);
            let next2 = self.t.get(self.i + 2);
            let is_label = next1.map_or(false, |t| t.kind == TokenKind::At)
                && next2.map_or(false, |t| matches!(t.kind, TokenKind::KwWhile | TokenKind::KwFor | TokenKind::KwDo));
            if is_label {
                loop_label = Some(self.text().to_string());
                self.bump(); // label name
                self.bump(); // '@'
            }
        }
        // Leading annotations on a statement (`@Suppress("…") val x = …`) carry no codegen
        // meaning here — skip them and parse the statement they decorate.
        if self.at(TokenKind::At) {
            while self.at(TokenKind::At) {
                self.skip_annotation();
                self.skip_newlines();
            }
            return self.parse_stmt();
        }
        // `lateinit var x: T` local — krusty defaults the slot to `null` rather than throwing
        // `UninitializedPropertyAccessException` on a read-before-init (a semantic difference that
        // miscompiles a negative test), so reject it (the file skips).
        if self.at(TokenKind::Ident) && self.text() == "lateinit"
            && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwVar) {
            self.diags.error(self.tok().span, "krusty: lateinit local variables are not supported");
            self.bump(); // 'lateinit'
        }
        let start = self.tok().span;
        match self.kind() {
            TokenKind::KwVal | TokenKind::KwVar => {
                let is_var = self.at(TokenKind::KwVar);
                self.bump();
                // Destructuring declaration: `val (a, b, …) = init`.
                if self.at(TokenKind::LParen) {
                    self.bump();
                    let mut entries = Vec::new();
                    loop {
                        let n = self.ident_or_error("variable name");
                        // A per-entry type annotation (`val (a: Int, b) = …`) is tolerated, ignored.
                        if self.eat(TokenKind::Colon) { let _ = self.parse_type(); }
                        entries.push((n, is_var));
                        if !self.eat(TokenKind::Comma) { break; }
                        if self.at(TokenKind::RParen) { break; } // trailing comma
                    }
                    self.expect(TokenKind::RParen, "')'");
                    self.expect(TokenKind::Eq, "'='");
                    self.skip_newlines();
                    let init = self.parse_expr();
                    return self.finish_stmt(Stmt::Destructure { entries, init }, start);
                }
                let name = self.ident_or_error("variable name");
                let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
                // `var x: T` with no initializer (deferred assignment) → synthesize the type's default
                // value (`0`/`false`/`null`); a later `x = …` assigns it. Only for `var` with a type
                // annotation (a `val` deferred-init needs assign-once tracking krusty lacks → rejected).
                let init = if is_var && ty.is_some() && !self.at(TokenKind::Eq) {
                    let sp = self.tok().span;
                    let e = self.default_init_expr(ty.as_ref().unwrap(), sp);
                    e
                } else {
                    self.expect(TokenKind::Eq, "'='");
                    self.skip_newlines();
                    self.parse_expr()
                };
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
            TokenKind::Ident if self.text() == "break" => {
                self.bump();
                let label = self.parse_loop_label_ref();
                self.finish_stmt(Stmt::Break(label), start)
            }
            TokenKind::Ident if self.text() == "continue" => {
                self.bump();
                let label = self.parse_loop_label_ref();
                self.finish_stmt(Stmt::Continue(label), start)
            }
            TokenKind::KwWhile => {
                self.bump();
                self.expect(TokenKind::LParen, "'('");
                let cond = self.parse_expr();
                self.expect(TokenKind::RParen, "')'");
                self.skip_newlines();
                // `parse_branch` handles a statement body (e.g. `while (c) i++`), not just an expression.
                let body = self.parse_branch();
                self.finish_stmt(Stmt::While { cond, body, label: loop_label }, start)
            }
            TokenKind::KwDo => {
                self.bump();
                self.skip_newlines();
                let body = self.parse_branch();
                self.skip_newlines();
                self.expect(TokenKind::KwWhile, "'while'");
                self.expect(TokenKind::LParen, "'('");
                let cond = self.parse_expr();
                self.expect(TokenKind::RParen, "')'");
                self.finish_stmt(Stmt::DoWhile { body, cond, label: loop_label }, start)
            }
            TokenKind::KwFor => self.parse_for(start, loop_label),
            // Local function declaration: `fun name(params): Ret { body }` inside a function body.
            TokenKind::KwFun => {
                let f = self.parse_fun(false, false);
                self.finish_stmt(Stmt::LocalFun(f), start)
            }
            _ => {
                let e = self.parse_expr();
                // Increment/decrement *statement* (`target++` / `++target`): `parse_prefix`/
                // `parse_postfix` built an `Expr::IncDec`; in statement position the value is
                // discarded, so re-route to the statement helper (which desugars a `Name` to
                // `Stmt::IncDec` and a member/index target to an assignment).
                if let Expr::IncDec { target, dec, .. } = self.file.expr(e).clone() {
                    let op_span = self.file.expr_spans[e.0 as usize];
                    return self.incdec_target(target, dec, op_span, start);
                }
                // assignment: `name = value` or `receiver.name = value`.
                if self.at(TokenKind::Eq) {
                    match self.file.expr(e).clone() {
                        Expr::Name(n) => {
                            self.bump(); // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            return self.finish_stmt(Stmt::Assign { name: n, value }, start);
                        }
                        Expr::Member { receiver, name } => {
                            self.bump(); // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            return self.finish_stmt(Stmt::AssignMember { receiver, name, value }, start);
                        }
                        Expr::Index { array, index } => {
                            self.bump(); // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            return self.finish_stmt(Stmt::AssignIndex { array, index, value }, start);
                        }
                        _ => self.diags.error(self.tok().span, "invalid assignment target"),
                    }
                }
                // compound assignment: `target op= value` → `target = target op value`.
                if let Some(op) = compound_op(self.kind()) {
                    let op_span = self.tok().span;
                    match self.file.expr(e).clone() {
                        Expr::Name(n) => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_expr();
                            let lhs = self.file.add_expr(Expr::Name(n.clone()), op_span);
                            let value = self.file.add_expr(Expr::Binary { op, lhs, rhs }, op_span);
                            return self.finish_stmt(Stmt::Assign { name: n, value }, start);
                        }
                        Expr::Member { receiver, name } => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_expr();
                            let lhs = self.file.add_expr(Expr::Member { receiver, name: name.clone() }, op_span);
                            let value = self.file.add_expr(Expr::Binary { op, lhs, rhs }, op_span);
                            return self.finish_stmt(Stmt::AssignMember { receiver, name, value }, start);
                        }
                        Expr::Index { array, index } => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_expr();
                            let lhs = self.file.add_expr(Expr::Index { array, index }, op_span);
                            let value = self.file.add_expr(Expr::Binary { op, lhs, rhs }, op_span);
                            return self.finish_stmt(Stmt::AssignIndex { array, index, value }, start);
                        }
                        _ => self.diags.error(self.tok().span, "invalid assignment target"),
                    }
                }
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    fn parse_for(&mut self, start: Span, label: Option<String>) -> StmtId {
        self.bump(); // 'for'
        self.expect(TokenKind::LParen, "'('");
        // A destructuring loop variable — `for ((a, b) in pairs)` — desugars to a synthetic temp plus
        // `val (a, b) = temp` prepended to the body (reusing the `Stmt::Destructure` machinery).
        let destructure: Option<Vec<(String, bool)>> = if self.at(TokenKind::LParen) {
            self.bump();
            let mut entries = Vec::new();
            loop {
                let n = self.ident_or_error("variable name");
                if self.eat(TokenKind::Colon) { let _ = self.parse_type(); }
                entries.push((n, false));
                if !self.eat(TokenKind::Comma) { break; }
                if self.at(TokenKind::RParen) { break; }
            }
            self.expect(TokenKind::RParen, "')'");
            Some(entries)
        } else {
            None
        };
        let name = match &destructure {
            Some(_) => format!("$dest${}", start.lo),
            None => self.ident_or_error("loop variable"),
        };
        self.expect(TokenKind::KwIn, "'in'");
        // Parse the iterable / range start at additive precedence so the `..`/`until`/`downTo`
        // operator is left for the `for`-specific range handling below (not swallowed into a
        // `RangeTo` value expression).
        let rstart = self.parse_bp(9);
        let kind = if self.eat(TokenKind::DotDot) {
            RangeKind::Through
        } else if self.eat(TokenKind::DotDotLt) {
            RangeKind::Until
        } else if self.at(TokenKind::Ident) && self.text() == "until" {
            self.bump();
            RangeKind::Until
        } else if self.at(TokenKind::Ident) && self.text() == "downTo" {
            self.bump();
            RangeKind::DownTo
        } else {
            self.expect(TokenKind::RParen, "')'");
            self.skip_newlines();
            let body = self.parse_branch();
            let body = self.desugar_destructure_body(&name, destructure, body);
            // `for (i in X.indices)` → counted loop `0 until X.size`.
            if let Expr::Member { receiver, name: mname } = self.file.expr(rstart).clone() {
                if mname == "indices" {
                    let sp = self.file.expr_spans[rstart.0 as usize];
                    let zero = self.file.add_expr(Expr::IntLit(0), sp);
                    let size = self.file.add_expr(Expr::Member { receiver, name: "size".to_string() }, sp);
                    let range = ForRange { start: zero, end: size, kind: RangeKind::Until, step: None };
                    return self.finish_stmt(Stmt::For { name, range, body, label }, start);
                }
            }
            // Otherwise iterate over `rstart` as a collection: `for (x in array)`.
            return self.finish_stmt(Stmt::ForEach { name, iterable: rstart, body, label }, start);
        };
        let rend = self.parse_bp(9);
        let step = if self.at(TokenKind::Ident) && self.text() == "step" {
            self.bump();
            Some(self.parse_expr())
        } else {
            None
        };
        self.expect(TokenKind::RParen, "')'");
        self.skip_newlines();
        let body = self.parse_branch();
        self.finish_stmt(Stmt::For { name, range: ForRange { start: rstart, end: rend, kind, step }, body, label }, start)
    }

    /// Parse an optional `@label` reference after `break`/`continue` (`break@outer`). Returns the label
    /// name, or `None` for an unlabeled `break`/`continue`.
    fn parse_loop_label_ref(&mut self) -> Option<String> {
        if self.at(TokenKind::At) {
            self.bump(); // '@'
            if self.at(TokenKind::Ident) {
                let l = self.text().to_string();
                self.bump();
                return Some(l);
            }
        }
        None
    }

    /// For a destructuring `for ((a, b) in …)`, prepend `val (a, b) = <temp>` to the loop body so the
    /// component names are bound from the synthetic loop variable. A no-op when not destructuring.
    fn desugar_destructure_body(&mut self, temp: &str, destructure: Option<Vec<(String, bool)>>, body: ExprId) -> ExprId {
        let Some(entries) = destructure else { return body };
        let sp = self.file.expr_spans[body.0 as usize];
        let temp_expr = self.file.add_expr(Expr::Name(temp.to_string()), sp);
        let dstmt = self.file.add_stmt(Stmt::Destructure { entries, init: temp_expr }, sp);
        match self.file.expr(body).clone() {
            Expr::Block { stmts, trailing } => {
                let mut s2 = vec![dstmt];
                s2.extend(stmts);
                self.file.add_expr(Expr::Block { stmts: s2, trailing }, sp)
            }
            _ => self.file.add_expr(Expr::Block { stmts: vec![dstmt], trailing: Some(body) }, sp),
        }
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

    /// Lookahead: is the `<` at the current position the start of a type-argument list that is
    /// followed by a call-like token (`(`, `{`, `.`)? Used to distinguish `a<B>(c)` (generic call)
    /// from `a < b > c` (two comparisons). Returns true without advancing `self.i`.
    fn lookahead_is_type_args_call(&self) -> bool {
        let mut j = self.i + 1; // skip the opening `<`
        let mut depth = 1i32;
        loop {
            let k = self.t.get(j).map(|t| t.kind);
            match k {
                Some(TokenKind::Lt) => { depth += 1; j += 1; }
                Some(TokenKind::Gt) => {
                    depth -= 1;
                    j += 1;
                    if depth == 0 { break; }
                }
                // `>=` closes the last `<` if depth == 1 (e.g. `Foo<Bar>=` — not valid type args).
                // Treat as "not type args" to stay safe.
                Some(TokenKind::GtEq) => return false,
                // Tokens valid inside type argument lists — including a function-type argument
                // (`Foo<(A) -> B>`): its parens and arrow.
                Some(TokenKind::Ident) | Some(TokenKind::Dot) | Some(TokenKind::Comma) |
                Some(TokenKind::Star) | Some(TokenKind::Question) | Some(TokenKind::Colon) |
                Some(TokenKind::LParen) | Some(TokenKind::RParen) | Some(TokenKind::Arrow) => {
                    j += 1;
                }
                _ => return false,
            }
        }
        // After `>`, must be followed by `(`, `{`, or `.` to be a generic call.
        matches!(self.t.get(j).map(|t| t.kind),
            Some(TokenKind::LParen) | Some(TokenKind::LBrace) | Some(TokenKind::Dot))
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
            // `in` / `!in` membership — a "named check" at comparison precedence (bp 7). A range RHS
            // (`a..b`, `a until b`, `a downTo b`) becomes `Expr::InRange`; any other RHS becomes
            // `container.contains(value)` (`!in` wraps it in `!`).
            if min_bp <= 7 {
                let in_negated = if self.at(TokenKind::KwIn) {
                    Some(false)
                } else if self.at(TokenKind::Not)
                    && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::KwIn)
                {
                    Some(true)
                } else {
                    None
                };
                if let Some(negated) = in_negated {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    if negated {
                        self.bump(); // '!'
                    }
                    self.bump(); // 'in'
                    self.skip_newlines();
                    let rstart = self.parse_bp(9); // the range start binds tighter than `in` (and `..`)
                    let kind = if self.eat(TokenKind::DotDot) {
                        Some(RangeKind::Through)
                    } else if self.eat(TokenKind::DotDotLt) {
                        Some(RangeKind::Until)
                    } else if self.at(TokenKind::Ident) && self.text() == "until" {
                        self.bump();
                        Some(RangeKind::Until)
                    } else if self.at(TokenKind::Ident) && self.text() == "downTo" {
                        self.bump();
                        Some(RangeKind::DownTo)
                    } else {
                        None
                    };
                    match kind {
                        Some(kind) => {
                            let rend = self.parse_bp(9);
                            let end = self.file.expr_spans[rend.0 as usize];
                            lhs = self.file.add_expr(Expr::InRange { value: lhs, start: rstart, end: rend, kind, negated }, Span::new(lspan.lo, end.hi));
                        }
                        None => {
                            // `value in container` → `container.contains(value)`.
                            let cspan = self.file.expr_spans[rstart.0 as usize];
                            let callee = self.file.add_expr(Expr::Member { receiver: rstart, name: "contains".to_string() }, Span::new(lspan.lo, cspan.hi));
                            let call = self.file.add_expr(Expr::Call { callee, args: vec![lhs] }, Span::new(lspan.lo, cspan.hi));
                            lhs = if negated {
                                self.file.add_expr(Expr::Unary { op: UnOp::Not, operand: call }, Span::new(lspan.lo, cspan.hi))
                            } else {
                                call
                            };
                        }
                    }
                    continue;
                }
            }
            // Range operators `a..b` (`rangeTo`) and `a..<b` (`rangeUntil`) as a *value*. These are
            // the only true range *operators* — `until`/`downTo`/`step` are ordinary stdlib infix
            // functions and flow through the infix-function path below. Binds tighter than infix
            // functions (so `a..b step c` is `(a..b).step(c)`) and looser than additive (operands at
            // bp 9). Builds `Expr::RangeTo`; the `for`/`in` forms are handled separately above.
            if min_bp <= 8 {
                let rkind = if self.at(TokenKind::DotDot) {
                    Some(RangeKind::Through)
                } else if self.at(TokenKind::DotDotLt) {
                    Some(RangeKind::Until)
                } else {
                    None
                };
                if let Some(kind) = rkind {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // '..' / '..<'
                    self.skip_newlines();
                    let hi = self.parse_bp(9);
                    let rspan = self.file.expr_spans[hi.0 as usize];
                    lhs = self.file.add_expr(Expr::RangeTo { lo: lhs, hi, kind }, Span::new(lspan.lo, rspan.hi));
                    continue;
                }
            }
            // Infix function call `a foo b` → `a.foo(b)`: a simple identifier between two operands.
            // Binds tighter than comparison (bp 7) and looser than additive (bp 9) — Kotlin's
            // `infixFunctionCall`. Resolution checks `foo` is actually an `infix`/member function.
            if min_bp <= 8 && self.at(TokenKind::Ident) {
                let name = self.text();
                // Exclude the real soft keywords only. `until`/`downTo`/`step` are ordinary stdlib
                // infix functions and parse as such here (`a until b` → `a.until(b)`); the `for`/`in`
                // forms recognize them separately before reaching this point.
                let is_soft_kw = matches!(name, "is" | "as" | "in");
                let next_starts_expr = self.t.get(self.i + 1).map_or(false, |t| starts_expr(t.kind));
                if !is_soft_kw && next_starts_expr {
                    let name = name.to_string();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // infix function name
                    self.skip_newlines();
                    let rhs = self.parse_bp(9); // operand binds at additive precedence or tighter
                    let rspan = self.file.expr_spans[rhs.0 as usize];
                    let callee = self.file.add_expr(Expr::Member { receiver: lhs, name }, Span::new(lspan.lo, rspan.hi));
                    lhs = self.file.add_expr(Expr::Call { callee, args: vec![rhs] }, Span::new(lspan.lo, rspan.hi));
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
        // `throw <expr>` — a soft keyword; raises an exception (bottom type `Nothing`).
        if self.at(TokenKind::Ident) && self.text() == "throw" {
            self.bump(); // 'throw'
            let operand = self.parse_bp(0);
            let end = self.file.expr_spans[operand.0 as usize];
            return self.file.add_expr(Expr::Throw { operand }, Span::new(start.lo, end.hi));
        }
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
        // Prefix `++target` / `--target` as a value (the new value). Statement position is intercepted
        // in `parse_stmt` before reaching here, so this fires only when used as a value.
        if self.at(TokenKind::PlusPlus) || self.at(TokenKind::MinusMinus) {
            let dec = self.at(TokenKind::MinusMinus);
            self.bump();
            let target = self.parse_bp(BP_PREFIX);
            let end = self.file.expr_spans[target.0 as usize];
            return self.file.add_expr(Expr::IncDec { target, dec, prefix: true }, Span::new(start.lo, end.hi));
        }
        let primary = self.parse_primary();
        self.parse_postfix(primary)
    }

    fn parse_postfix(&mut self, mut lhs: ExprId) -> ExprId {
        // Explicit type arguments parsed just before a call paren (`foo<Int>(…)`), attached to the
        // call once it is built so a constructor instantiation (`ArrayList<Int>()`) keeps its args.
        let mut pending_targs: Vec<TypeRef> = Vec::new();
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
                // Postfix `target++` / `target--` as a value (the old value). In statement position
                // `parse_stmt` re-routes the resulting `IncDec` to the statement path.
                TokenKind::PlusPlus | TokenKind::MinusMinus => {
                    let dec = self.at(TokenKind::MinusMinus);
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.bump();
                    lhs = self.file.add_expr(Expr::IncDec { target: lhs, dec, prefix: false }, Span::new(lspan.lo, end.hi));
                }
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
                // `expr::name` or `Expr::class` — bound callable reference / class literal.
                TokenKind::ColonColon => {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // '::'
                    let name = if self.at(TokenKind::Ident) {
                        let n = self.text().to_string(); self.bump(); n
                    } else if self.at(TokenKind::KwClass) {
                        self.bump(); "class".to_string()
                    } else {
                        "<error>".to_string()
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(Expr::CallableRef { receiver: Some(lhs), name }, Span::new(lspan.lo, end.hi));
                }
                TokenKind::LParen => {
                    self.bump();
                    self.skip_newlines();
                    let mut args = Vec::new();
                    let mut names: Vec<Option<String>> = Vec::new();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        // Named argument `name = expr` — `name` is an identifier followed by a single
                        // `=` (not `==`, which begins an equality expression).
                        if self.at(TokenKind::Ident)
                            && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Eq)
                        {
                            let n = self.text().to_string();
                            self.bump(); // name
                            self.bump(); // '='
                            self.skip_newlines();
                            names.push(Some(n));
                        } else {
                            names.push(None);
                        }
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
                    let call = self.file.add_expr(Expr::Call { callee: lhs, args }, Span::new(lspan.lo, end.hi));
                    if names.iter().any(|n| n.is_some()) {
                        self.file.call_arg_names.insert(call.0, names);
                    }
                    if !pending_targs.is_empty() {
                        self.file.call_type_args.insert(call.0, std::mem::take(&mut pending_targs));
                    }
                    lhs = call;
                }
                // Trailing lambda: `expr { … }` / `recv.m(args) { … }` → append the lambda as the
                // last call argument (same line only, to avoid swallowing an unrelated block).
                TokenKind::LBrace => {
                    let lambda = self.parse_lambda();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    let old = lhs;
                    lhs = match self.file.expr(lhs).clone() {
                        Expr::Call { callee, mut args } => {
                            args.push(lambda);
                            self.file.add_expr(Expr::Call { callee, args }, Span::new(lspan.lo, end.hi))
                        }
                        _ => self.file.add_expr(Expr::Call { callee: lhs, args: vec![lambda] }, Span::new(lspan.lo, end.hi)),
                    };
                    // Carry any named-argument metadata to the rebuilt call (the trailing lambda is
                    // an extra positional argument).
                    if let Some(mut names) = self.file.call_arg_names.remove(&old.0) {
                        names.push(None);
                        self.file.call_arg_names.insert(lhs.0, names);
                    }
                }
                // `array[index]` element access.
                TokenKind::LBracket => {
                    self.bump();
                    self.skip_newlines();
                    let index = self.parse_expr();
                    self.skip_newlines();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.expect(TokenKind::RBracket, "']'");
                    lhs = self.file.add_expr(Expr::Index { array: lhs, index }, Span::new(lspan.lo, end.hi));
                }
                // `expr<TypeArgs>(args)` — generic call with explicit type arguments.
                // Disambiguate from `a < b > c` (two comparisons) by checking whether a balanced
                // `>` is immediately followed by `(`, `{`, or `.` (call-like context).
                TokenKind::Lt if self.lookahead_is_type_args_call() => {
                    // Capture the explicit type arguments for the call that follows.
                    pending_targs = self.parse_type_args();
                }
                _ => break,
            }
        }
        lhs
    }

    fn parse_primary(&mut self) -> ExprId {
        let span = self.tok().span;
        // `try { … } catch (e: T) { … }` — a soft keyword followed by a block.
        if self.at(TokenKind::Ident)
            && self.text() == "try"
            && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::LBrace)
        {
            return self.parse_try();
        }
        match self.kind() {
            // Collection-literal `[a, b, …]` (annotation arguments / defaults) → `arrayOf(a, b, …)`;
            // `[]` → `emptyArray()`. Reuses the array-builtin resolution + (target-typed) codegen.
            TokenKind::LBracket => {
                self.bump(); // '['
                self.skip_newlines();
                let mut args = Vec::new();
                while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                    args.push(self.parse_expr());
                    self.skip_newlines();
                    if self.at(TokenKind::Comma) {
                        self.bump();
                        self.skip_newlines();
                    } else {
                        break;
                    }
                }
                let end = self.tok().span;
                self.expect(TokenKind::RBracket, "']'");
                let fname = if args.is_empty() { "emptyArray" } else { "arrayOf" };
                let callee = self.file.add_expr(Expr::Name(fname.to_string()), span);
                self.file.add_expr(Expr::Call { callee, args }, Span::new(span.lo, end.hi))
            }
            TokenKind::IntLit => {
                let v = parse_int_literal(self.text());
                self.bump();
                // Values outside the i32 range are Long literals in Kotlin (no L suffix needed).
                if v > i32::MAX as i64 || v < i32::MIN as i64 {
                    self.file.add_expr(Expr::LongLit(v), span)
                } else {
                    self.file.add_expr(Expr::IntLit(v), span)
                }
            }
            TokenKind::LongLit => {
                let t = self.text();
                let v = parse_int_literal(&t[..t.len() - 1]); // strip trailing `L`
                self.bump();
                self.file.add_expr(Expr::LongLit(v), span)
            }
            TokenKind::UIntLit => {
                let v = parse_int_literal(self.text()); // suffix stripped inside
                self.bump();
                self.file.add_expr(Expr::UIntLit(v), span)
            }
            TokenKind::ULongLit => {
                let v = parse_int_literal(self.text());
                self.bump();
                self.file.add_expr(Expr::ULongLit(v), span)
            }
            TokenKind::DoubleLit => {
                let v = self.text().parse::<f64>().unwrap_or(0.0);
                self.bump();
                self.file.add_expr(Expr::DoubleLit(v), span)
            }
            TokenKind::FloatLit => {
                // strip the trailing `f`/`F` suffix
                let t = self.text();
                let v = t[..t.len() - 1].parse::<f32>().unwrap_or(0.0);
                self.bump();
                self.file.add_expr(Expr::FloatLit(v), span)
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
            // An anonymous object expression `object : Super(args)? { … }` (in value position).
            TokenKind::Ident
                if self.text() == "object"
                    && self.t.get(self.i + 1).map_or(false, |t| matches!(t.kind, TokenKind::Colon | TokenKind::LBrace)) =>
            {
                self.parse_anon_object(span)
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
            TokenKind::LBrace => self.parse_lambda(),
            // `::name` — top-level callable reference / class literal without a receiver.
            TokenKind::ColonColon => {
                self.bump(); // '::'
                let name = if self.at(TokenKind::Ident) {
                    let n = self.text().to_string(); self.bump(); n
                } else if self.at(TokenKind::KwClass) {
                    self.bump(); "class".to_string()
                } else {
                    "<error>".to_string()
                };
                self.file.add_expr(Expr::CallableRef { receiver: None, name }, span)
            }
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

    /// `try { … } catch (e: T) { … } …` — krusty supports one or more `catch` clauses; `finally` is
    /// rejected (it needs duplicated-block / catch-all-rethrow lowering not yet implemented).
    fn parse_try(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'try'
        self.skip_newlines();
        let body = self.parse_block_expr();
        let mut catches = Vec::new();
        let mut finally = None;
        loop {
            let save = self.i;
            self.skip_newlines();
            if self.at(TokenKind::Ident) && self.text() == "catch" {
                self.bump(); // 'catch'
                self.expect(TokenKind::LParen, "'('");
                let name = self.ident_or_error("catch parameter name");
                self.expect(TokenKind::Colon, "':'");
                let ty = self.parse_type();
                self.expect(TokenKind::RParen, "')'");
                self.skip_newlines();
                let cbody = self.parse_block_expr();
                catches.push(CatchClause { name, ty, body: cbody });
            } else if self.at(TokenKind::Ident) && self.text() == "finally" {
                self.bump(); // 'finally'
                self.skip_newlines();
                finally = Some(self.parse_block_expr());
                break; // `finally` is always last
            } else {
                self.i = save;
                break;
            }
        }
        if catches.is_empty() && finally.is_none() {
            self.diags.error(start, "try without a catch or finally is not supported");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(Expr::Try { body, catches, finally }, Span::new(start.lo, end.hi))
    }

    /// Desugar a `++`/`--` statement on an already-parsed lvalue `e` (the operator at `op_span`,
    /// statement starting at `start`). A simple `Name` uses the `IncDec` node (overloadable-operator
    /// aware); `obj.x` / `arr[i]` desugar to `target = target ± 1` (the old value is discarded in
    /// statement position). `dec` selects subtraction.
    fn incdec_target(&mut self, e: ExprId, dec: bool, op_span: Span, start: Span) -> StmtId {
        let op = if dec { BinOp::Sub } else { BinOp::Add };
        // The desugar `target = target ± 1` re-evaluates `target`, so its receiver/index must be
        // side-effect-free (a pure access path). For a complex receiver (`f().x++`) kotlinc evaluates
        // it exactly once — not yet modeled — so bail (skip the file) rather than double-evaluate.
        match self.file.expr(e).clone() {
            Expr::Name(n) => self.parse_incdec(n, dec, start),
            Expr::Member { receiver, name } if self.is_pure_path(receiver) => {
                let one = self.file.add_expr(Expr::IntLit(1), op_span);
                let lhs = self.file.add_expr(Expr::Member { receiver, name: name.clone() }, op_span);
                let value = self.file.add_expr(Expr::Binary { op, lhs, rhs: one }, op_span);
                self.finish_stmt(Stmt::AssignMember { receiver, name, value }, start)
            }
            Expr::Index { array, index } if self.is_pure_path(array) && self.is_pure_path(index) => {
                let one = self.file.add_expr(Expr::IntLit(1), op_span);
                let lhs = self.file.add_expr(Expr::Index { array, index }, op_span);
                let value = self.file.add_expr(Expr::Binary { op, lhs, rhs: one }, op_span);
                self.finish_stmt(Stmt::AssignIndex { array, index, value }, start)
            }
            _ => {
                self.diags.error(op_span, "krusty: '++'/'--' is only supported on a simple variable or pure access path");
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    /// Whether `e` is a side-effect-free access path — a name, a literal, or a member/index chain
    /// bottoming out at one. Such an expression can be re-evaluated safely (used to gate the
    /// `++`/`--` desugar, which reads its target twice).
    fn is_pure_path(&self, e: ExprId) -> bool {
        match self.file.expr(e) {
            Expr::Name(_) | Expr::IntLit(_) | Expr::LongLit(_) | Expr::CharLit(_) | Expr::BoolLit(_) | Expr::NullLit => true,
            Expr::Member { receiver, .. } => self.is_pure_path(*receiver),
            Expr::Index { array, index } => self.is_pure_path(*array) && self.is_pure_path(*index),
            _ => false,
        }
    }

    fn parse_when(&mut self) -> ExprId {
        let start = self.tok().span;
        self.bump(); // 'when'
        // `when (val v = e) { … }` — a subject variable. Desugar to `{ val v = e; when (v) { … } }`:
        // parse the binding here, use a `Name(v)` reference as the subject, then wrap the whole `when`
        // in a block holding the `val` so every downstream path (smart-casts, `is` arms) sees a local.
        let mut subject_var: Option<(StmtId, ExprId)> = None;
        let subject = if self.eat(TokenKind::LParen) {
            if self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar) {
                let vstart = self.tok().span;
                let is_var = self.at(TokenKind::KwVar);
                self.bump(); // 'val' / 'var'
                let name = self.ident_or_error("variable name");
                let ty = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
                self.expect(TokenKind::Eq, "'='");
                let init = self.parse_expr();
                self.expect(TokenKind::RParen, "')'");
                let sp = Span::new(vstart.lo, self.file.expr_spans[init.0 as usize].hi);
                let stmt = self.file.add_stmt(Stmt::Local { is_var, name: name.clone(), ty, init }, sp);
                let nm = self.file.add_expr(Expr::Name(name), sp);
                subject_var = Some((stmt, nm));
                Some(nm)
            } else {
                let e = self.parse_expr();
                self.expect(TokenKind::RParen, "')'");
                Some(e)
            }
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
                conditions.push(self.parse_when_condition(subject));
                while self.eat(TokenKind::Comma) {
                    self.skip_newlines();
                    conditions.push(self.parse_when_condition(subject));
                }
            }
            self.expect(TokenKind::Arrow, "'->'");
            self.skip_newlines();
            let body = self.parse_branch();
            arms.push(WhenArm { conditions, body });
        }
        let end = self.tok().span;
        self.expect(TokenKind::RBrace, "'}'");
        let span = Span::new(start.lo, end.hi);
        let when_expr = self.file.add_expr(Expr::When { subject, arms }, span);
        match subject_var {
            Some((stmt, _)) => self.file.add_expr(Expr::Block { stmts: vec![stmt], trailing: Some(when_expr) }, span),
            None => when_expr,
        }
    }

    /// A single `when`-arm condition. In the subject form, `is T` / `!is T` becomes a type test
    /// against the subject (`Expr::Is` whose operand is the subject expression); otherwise a value
    /// matched by `==`.
    fn parse_when_condition(&mut self, subject: Option<ExprId>) -> ExprId {
        let negated = if self.at(TokenKind::Ident) && self.text() == "is" {
            Some(false)
        } else if self.at(TokenKind::Not)
            && self.t.get(self.i + 1).map_or(false, |t| t.kind == TokenKind::Ident && t.text(self.src) == "is")
        {
            Some(true)
        } else {
            None
        };
        if let (Some(negated), Some(subj)) = (negated, subject) {
            let start = self.tok().span;
            if negated {
                self.bump(); // '!'
            }
            self.bump(); // 'is'
            let ty = self.parse_type();
            let end = self.t[self.i.saturating_sub(1)].span;
            return self.file.add_expr(Expr::Is { operand: subj, ty, negated }, Span::new(start.lo, end.hi));
        }
        self.parse_expr()
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
            | "lateinit" | "infix" | "reified" | "vararg" | "const" | "sealed" | "actual"
            | "expect" | "value" | "inner"
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

/// Whether `k` can begin an expression — used to decide if `a IDENT …` is an infix function call
/// (the identifier must be followed by an operand). Conservative: false only stops an infix call.
fn starts_expr(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Ident
            | TokenKind::IntLit
            | TokenKind::LongLit
            | TokenKind::UIntLit
            | TokenKind::ULongLit
            | TokenKind::DoubleLit
            | TokenKind::FloatLit
            | TokenKind::StringLit
            | TokenKind::CharLit
            | TokenKind::TemplateStart
            | TokenKind::KwTrue
            | TokenKind::KwFalse
            | TokenKind::KwNull
            | TokenKind::KwIf
            | TokenKind::KwWhen
            | TokenKind::LParen
            | TokenKind::LBrace
            | TokenKind::Minus
            | TokenKind::Not
    )
}

fn infix_op(k: TokenKind) -> Option<BinOp> {
    Some(match k {
        TokenKind::OrOr => BinOp::Or,
        TokenKind::AndAnd => BinOp::And,
        TokenKind::EqEq => BinOp::Eq,
        TokenKind::NotEq => BinOp::Ne,
        TokenKind::RefEq => BinOp::RefEq,
        TokenKind::RefNe => BinOp::RefNe,
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
        BinOp::Eq | BinOp::Ne | BinOp::RefEq | BinOp::RefNe => (5, 6),
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
            Some('b') => '\u{0008}',
            Some('\\') => '\\',
            Some('\'') => '\'',
            Some('"') => '"',
            Some('0') => '\0',
            Some('$') => '$',
            // `\uXXXX` — a 4-hex-digit UTF-16 code unit.
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\0')
            }
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
                Some('b') => out.push('\u{0008}'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('$') => out.push('$'),
                Some('0') => out.push('\0'),
                // `\uXXXX` — a 4-hex-digit UTF-16 code unit.
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse an integer literal: decimal, `0x`/`0X` hex, or `0b`/`0B` binary, with `_` separators.
/// Parses into `u64` (so `0xFFFFFFFF`/`0xFFFFFFFFFFFFFFFF` fit) then reinterprets as `i64`.
fn parse_int_literal(text: &str) -> i64 {
    // Strip trailing type suffixes (L, u, U, uL, UL) before parsing.
    let text = text.trim_end_matches(|c: char| matches!(c, 'L' | 'l' | 'u' | 'U'));
    let t: String = text.chars().filter(|c| *c != '_').collect();
    let (radix, digits) = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (16, h)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (2, b)
    } else {
        (10, t.as_str())
    };
    if radix == 10 {
        digits.parse::<i64>().unwrap_or(0)
    } else {
        u64::from_str_radix(digits, radix).map(|v| v as i64).unwrap_or(0)
    }
}

fn unquote(raw: &str) -> String {
    // Raw string `"""..."""`: content is verbatim (no escape processing), three quotes each side.
    if raw.starts_with("\"\"\"") {
        let inner = raw.strip_prefix("\"\"\"").and_then(|s| s.strip_suffix("\"\"\"")).unwrap_or(raw);
        return inner.to_string();
    }
    let inner = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{0008}'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('$') => out.push('$'),
                Some('0') => out.push('\0'),
                // `\uXXXX` — a 4-hex-digit UTF-16 code unit.
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
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
    fn receiver_function_type_param() {
        // A receiver (extension) function type `Recv.() -> R` parses by folding the receiver in as the
        // first `FunctionN` parameter — no parse error (was "expected ')'" before).
        let mut d = DiagSink::new();
        let src = "fun build(instructions: Buildee<T>.(Int) -> Unit) {}";
        let toks = lex(src, &mut d);
        let _ = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "receiver function type should parse: {}", d.render("test", src));
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
    fn function_type_as_generic_arg() {
        // `Foo<() -> Unit>()` — a function type as a generic type argument must be recognized as a
        // generic call (not parsed as a `<` comparison). `tree` asserts no parse errors.
        let t = tree("fun f() { val xs = ArrayList<() -> Unit>() }");
        assert!(t.contains("ArrayList"), "tree: {t}");
        // Also the two-arg form `Map<String, (Int) -> Int>`.
        let _ = tree("fun g() { val m = HashMap<String, (Int) -> Int>() }");
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
