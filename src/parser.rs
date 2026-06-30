//! Stage B: tokens → arena AST. Recursive descent for decls/stmts, Pratt for expressions with the
//! Kotlin precedence table. Newlines (their own token) act as statement/expression terminators;
//! they are skipped after binary operators and between statements/declarations.

use crate::ast::*;
use crate::diag::{DiagSink, Span};
use crate::features::LangFeatures;
use crate::token::{Token, TokenKind};

/// Parse with the default (no experimental flags) language feature set.
pub fn parse(src: &str, tokens: &[Token], diags: &mut DiagSink) -> File {
    parse_with_features(src, tokens, diags, &LangFeatures::default())
}

/// Parse under an explicit language-feature set — flag-gated syntax (e.g. name-based `[a, b]`
/// destructuring) is accepted only when its feature is enabled, matching a drop-in `kotlinc`.
pub fn parse_with_features(
    src: &str,
    tokens: &[Token],
    diags: &mut DiagSink,
    features: &LangFeatures,
) -> File {
    let mut p = Parser {
        src,
        t: tokens,
        i: 0,
        file: File::default(),
        diags,
        name_based_destructuring: features.has("NameBasedDestructuring"),
        no_trailing_lambda: false,
        lexical_type_params: Vec::new(),
        lexical_type_param_bounds: Vec::new(),
        pending_annotations: Vec::new(),
        pending_annotation_args: Vec::new(),
    };
    p.parse_file();
    p.file.assert_always_enabled = features.has("AssertionsAlwaysEnable");
    p.file.assert_always_disabled = features.has("AssertionsAlwaysDisable");
    hoist_local_classes(&mut p.file);
    fixup_parenless_base_classes(&mut p.file);
    rewrite_anon_captures(&mut p.file);
    p.file
}

/// Whether `body` is the FunBody root expression (an `=`-body or a block).
fn fun_body_root(body: &FunBody) -> Option<ExprId> {
    match body {
        FunBody::Expr(e) | FunBody::Block(e) => Some(*e),
        FunBody::None => None,
    }
}

/// Whether `t` (or any of its type arguments / function-type parts) names one of `tps`.
/// Replace every enclosing type-parameter name in `t` with `Any` (its erased upper bound), recursing
/// through type arguments and function-type parameter/return positions. A captured value whose declared
/// type mentions an enclosing type parameter (`x: (T) -> Unit`) becomes a synth-class field of the erased
/// type (`(Any) -> Unit`, i.e. `Function1`) — sound because krusty erases generics to `Any`/`Object`
/// throughout, so the field, constructor argument, and body use all agree on the erased type.
fn erase_type_params(t: &TypeRef, tps: &std::collections::HashSet<String>) -> TypeRef {
    if tps.contains(&t.name) {
        return TypeRef {
            name: "Any".to_string(),
            nullable: t.nullable,
            arg: None,
            targs: Vec::new(),
            span: t.span,
            fun_params: Vec::new(),
            fun_has_receiver: false,
            fun_suspend: false,
        };
    }
    TypeRef {
        arg: t
            .arg
            .as_deref()
            .map(|a| Box::new(erase_type_params(a, tps))),
        targs: t.targs.iter().map(|a| erase_type_params(a, tps)).collect(),
        fun_params: t
            .fun_params
            .iter()
            .map(|a| erase_type_params(a, tps))
            .collect(),
        ..t.clone()
    }
}

/// Names BOUND inside the anonymous class `did` (its constructor properties, body properties, and the
/// names/params of its methods) — references to these are NOT captures of the enclosing scope.
fn anon_bound_names(file: &File, did: DeclId) -> std::collections::HashSet<String> {
    let mut s = std::collections::HashSet::new();
    if let Decl::Class(c) = file.decl(did) {
        for p in &c.props {
            s.insert(p.name.clone());
        }
        for bp in &c.body_props {
            s.insert(bp.name.clone());
        }
        for m in &c.methods {
            s.insert(m.name.clone());
            for p in &m.params {
                s.insert(p.name.clone());
            }
        }
    }
    s
}

/// Map the parsed class modifiers to a [`Modality`]. `sealed` wins (it implies abstract+open), then
/// `abstract`, then `open`, else `final`.
fn modality_of(is_open: bool, is_abstract: bool, is_sealed: bool) -> crate::ast::Modality {
    use crate::ast::Modality;
    if is_sealed {
        Modality::Sealed
    } else if is_abstract {
        Modality::Abstract
    } else if is_open {
        Modality::Open
    } else {
        Modality::Final
    }
}

/// A non-nullable, non-generic type reference to a simple type name (for a literal-inferred local).
fn simple_type_ref(name: &str, span: crate::diag::Span) -> TypeRef {
    TypeRef {
        name: name.to_string(),
        nullable: false,
        arg: None,
        targs: Vec::new(),
        span,
        fun_params: Vec::new(),
        fun_has_receiver: false,
        fun_suspend: false,
    }
}

/// The type of a local whose initializer is a literal — recoverable WITHOUT inference. Returns `None`
/// for any non-literal initializer (then the local is not a slice-1/2 capture candidate).
fn literal_init_type(file: &File, init: ExprId) -> Option<TypeRef> {
    let span = file.expr_spans[init.0 as usize];
    let name = match file.expr(init) {
        Expr::IntLit(_) => "Int",
        Expr::LongLit(_) => "Long",
        Expr::DoubleLit(_) => "Double",
        Expr::FloatLit(_) => "Float",
        Expr::BoolLit(_) => "Boolean",
        Expr::CharLit(_) => "Char",
        Expr::UIntLit(_) => "UInt",
        Expr::ULongLit(_) => "ULong",
        Expr::StringLit(_) | Expr::Template(_) => "String",
        _ => return None,
    };
    Some(simple_type_ref(name, span))
}

/// Collect read-capturable LOCALS (`val`/`var name (: T)? = init`) declared anywhere in `root`, as
/// (name, type) where the type is the explicit annotation or a literal-inferred type. A local with a
/// non-literal, unannotated initializer is omitted (no inference available here).
fn collect_locals(file: &File, root: ExprId, out: &mut Vec<(String, TypeRef)>) {
    if let Expr::Block { stmts, .. } = file.expr(root) {
        for &s in stmts {
            if let Stmt::Local { name, ty, init, .. } = file.stmt(s) {
                if let Some(t) = ty.clone().or_else(|| literal_init_type(file, *init)) {
                    out.push((name.clone(), t));
                }
            }
        }
    }
    let cell = std::cell::RefCell::new(out);
    file.any_child_expr(
        root,
        &mut |c| {
            collect_locals(file, c, &mut cell.borrow_mut());
            false
        },
        &mut |s| {
            file.any_child_stmt(s, &mut |c| {
                collect_locals(file, c, &mut cell.borrow_mut());
                false
            });
            false
        },
    );
}

/// Whether the anonymous class `did`'s body WRITES the name `n` (`n = …` / `n++`). A written capture
/// needs a shared `Ref` cell (kotlinc's boxing) to stay observable in the enclosing scope — not modeled
/// in this slice — so such a name is NOT captured by value.
fn anon_body_writes(file: &File, did: DeclId, n: &str) -> bool {
    let Decl::Class(c) = file.decl(did) else {
        return false;
    };
    fn writes(file: &File, root: ExprId, n: &str) -> bool {
        let here = matches!(file.expr(root), Expr::Block { stmts, .. }
            if stmts.iter().any(|&s| matches!(file.stmt(s),
                Stmt::Assign { name, .. } | Stmt::IncDec { name, .. } if name == n)));
        if here {
            return true;
        }
        let found = std::cell::RefCell::new(false);
        file.any_child_expr(
            root,
            &mut |c| {
                if writes(file, c, n) {
                    *found.borrow_mut() = true;
                }
                false
            },
            &mut |s| {
                file.any_child_stmt(s, &mut |c| {
                    if writes(file, c, n) {
                        *found.borrow_mut() = true;
                    }
                    false
                });
                false
            },
        );
        found.into_inner()
    }
    let in_method = c
        .methods
        .iter()
        .filter_map(|m| fun_body_root(&m.body))
        .any(|root| writes(file, root, n));
    let in_prop = c
        .body_props
        .iter()
        .filter_map(|p| p.init)
        .any(|init| writes(file, init, n));
    let in_super = c.base_args.iter().any(|&a| writes(file, a, n));
    in_method || in_prop || in_super
}

/// Whether the anonymous class `did`'s body (method bodies, body-property initializers, super-call
/// arguments) reads the name `n`.
fn anon_body_uses(file: &File, did: DeclId, n: &str) -> bool {
    let Decl::Class(c) = file.decl(did) else {
        return false;
    };
    let in_method = c
        .methods
        .iter()
        .filter_map(|m| fun_body_root(&m.body))
        .any(|root| crate::resolve::expr_uses_name_pub(file, root, n));
    let in_prop = c
        .body_props
        .iter()
        .filter_map(|p| p.init)
        .any(|init| crate::resolve::expr_uses_name_pub(file, init, n));
    let in_super = c
        .base_args
        .iter()
        .any(|&a| crate::resolve::expr_uses_name_pub(file, a, n));
    in_method || in_prop || in_super
}

/// Collect the `ExprId`s of zero-argument constructions of an anonymous class (`Call{Name(anon), []}`)
/// reachable from `root`, paired with the anon class name.
fn collect_anon_calls(
    file: &File,
    root: ExprId,
    anon: &std::collections::HashMap<String, DeclId>,
    out: &mut Vec<(ExprId, String)>,
) {
    if let Expr::Call { callee, args } = file.expr(root) {
        if args.is_empty() {
            if let Expr::Name(name) = file.expr(*callee) {
                if anon.contains_key(name) {
                    out.push((root, name.clone()));
                }
            }
        }
    }
    let cell = std::cell::RefCell::new(out);
    file.any_child_expr(
        root,
        &mut |c| {
            collect_anon_calls(file, c, anon, &mut cell.borrow_mut());
            false
        },
        &mut |s| {
            file.any_child_stmt(s, &mut |c| {
                collect_anon_calls(file, c, anon, &mut cell.borrow_mut());
                false
            });
            false
        },
    );
}

/// Slice 1 of anonymous-object capture. An `object : I { … }` expression is desugared (in
/// `parse_anon_object`) to a hoisted top-level synth class + a no-argument construction, so a body that
/// reads an enclosing local fails to resolve. This rewrite turns each captured enclosing-FUNCTION
/// PARAMETER and read-only LOCAL into a constructor `val` property of the synth class and passes it at
/// the construction — after which the ordinary class machinery resolves the body reference as a member
/// and emits the field.
///
/// Captured types must be known WITHOUT inference: a parameter's declared type, or a local's explicit
/// annotation / literal-inferred type. A captured type that mentions an enclosing type parameter
/// (`x: (T) -> Unit`) is captured with that type parameter erased to `Any` (see `erase_type_params`).
/// These stay unresolved (→ the file cleanly skips, never a miscompile) and are NOT captured: a name
/// WRITTEN inside the anon (needs a shared `Ref` cell); an outer local with a non-literal, unannotated
/// initializer.
fn rewrite_anon_captures(file: &mut File) {
    // anon class simple name -> its DeclId
    let mut anon_by_name: std::collections::HashMap<String, DeclId> =
        std::collections::HashMap::new();
    for &d in &file.decls {
        if let Decl::Class(c) = file.decl(d) {
            if c.name.contains("$anon$") {
                anon_by_name.insert(c.name.clone(), d);
            }
        }
    }
    if anon_by_name.is_empty() {
        return;
    }

    // Each function body with its capturable (name, type) list — parameters plus read-capturable
    // locals — and the enclosing type-parameter names.
    type CaptureBody = (
        Vec<(String, TypeRef)>,
        std::collections::HashSet<String>,
        ExprId,
    );
    let mut fn_bodies: Vec<CaptureBody> = Vec::new();
    let mut push_body =
        |params: Vec<(String, TypeRef)>, tps: std::collections::HashSet<String>, root: ExprId| {
            let mut cands = params;
            let mut locals = Vec::new();
            collect_locals(file, root, &mut locals);
            cands.extend(locals);
            fn_bodies.push((cands, tps, root));
        };
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) => {
                if let Some(root) = fun_body_root(&f.body) {
                    let params = f
                        .params
                        .iter()
                        .map(|p| (p.name.clone(), p.ty.clone()))
                        .collect();
                    let tps = f.type_params.iter().cloned().collect();
                    push_body(params, tps, root);
                }
            }
            Decl::Class(c) => {
                let class_tps: std::collections::HashSet<String> =
                    c.type_params.iter().cloned().collect();
                for m in &c.methods {
                    if let Some(root) = fun_body_root(&m.body) {
                        let params = m
                            .params
                            .iter()
                            .map(|p| (p.name.clone(), p.ty.clone()))
                            .collect();
                        let mut tps = class_tps.clone();
                        tps.extend(m.type_params.iter().cloned());
                        push_body(params, tps, root);
                    }
                }
            }
            _ => {}
        }
    }

    let mut class_caps: Vec<(DeclId, Vec<(String, TypeRef)>)> = Vec::new();
    let mut call_args: Vec<(ExprId, Vec<String>)> = Vec::new();
    for (params, tps, root) in &fn_bodies {
        if params.is_empty() {
            continue;
        }
        let mut calls: Vec<(ExprId, String)> = Vec::new();
        collect_anon_calls(file, *root, &anon_by_name, &mut calls);
        for (call_id, anon_name) in calls {
            let did = anon_by_name[&anon_name];
            let bound = anon_bound_names(file, did);
            let mut caps: Vec<(String, TypeRef)> = Vec::new();
            for (pn, pty) in params {
                if bound.contains(pn) {
                    continue;
                }
                if caps.iter().any(|(n, _)| n == pn) {
                    continue; // a local shadowing a param — capture once
                }
                // A captured name WRITTEN inside the anon would need a shared `Ref` cell to stay
                // observable in the enclosing scope (not modeled here) — capturing it by value would
                // miscompile, so leave it (the reference stays unresolved → the file cleanly skips).
                if anon_body_writes(file, did, pn) {
                    continue;
                }
                if anon_body_uses(file, did, pn) {
                    // A captured type that mentions an enclosing type parameter (`x: (T) -> Unit`) is
                    // captured with the type parameter erased to `Any` — the synth-class field/ctor/use
                    // all agree on the erased type, matching krusty's generic erasure.
                    caps.push((pn.clone(), erase_type_params(pty, tps)));
                }
            }
            if caps.is_empty() {
                continue;
            }
            call_args.push((call_id, caps.iter().map(|(n, _)| n.clone()).collect()));
            class_caps.push((did, caps));
        }
    }

    for (did, caps) in class_caps {
        if let Decl::Class(c) = &mut file.decl_arena[did.0 as usize] {
            for (name, ty) in caps {
                if c.props.iter().any(|p| p.name == name) {
                    continue;
                }
                c.props.push(PropParam {
                    name,
                    ty,
                    is_var: false,
                    is_property: true,
                    default: None,
                    annotations: Vec::new(),
                    annotation_args: Vec::new(),
                });
            }
        }
    }
    for (call_id, names) in call_args {
        let span = file.expr_spans[call_id.0 as usize];
        let arg_ids: Vec<ExprId> = names
            .into_iter()
            .map(|n| file.add_expr(Expr::Name(n), span))
            .collect();
        if let Expr::Call { args, .. } = &mut file.expr_arena[call_id.0 as usize] {
            args.extend(arg_ids);
        }
    }
}

/// Hoist every local class (`class`/`interface`/… declared inside a function body, parsed as
/// `Stmt::LocalClass`) to a top-level `Decl::Class`, so signature collection, checking, and lowering
/// treat it like any other class — zero changes to those passes. The `Stmt::LocalClass` stays in the
/// body as a no-op. A local class that captures outer locals is checked with no enclosing scope, so its
/// outer references fail to resolve and the file cleanly skips (never miscompiles). Two same-named local
/// classes (or a clash with a top-level name) become a "conflicting declarations" skip — also sound.
fn hoist_local_classes(file: &mut File) {
    use crate::ast::{Decl, Stmt};
    let hoisted: Vec<crate::ast::ClassDecl> = file
        .stmt_arena
        .iter()
        .filter_map(|s| match s {
            // A local class with super-CONSTRUCTOR arguments (`class Z : C(s)`) can capture an outer
            // local through that call, which the hoisted (outer-scope-free) check doesn't currently
            // reject — so it would miscompile. Don't hoist those; the reference stays unresolved and the
            // file skips (sound). Local-class INHERITANCE is a later slice. Everything else hoists.
            Stmt::LocalClass(c) if c.base_args.is_empty() => Some(c.clone()),
            _ => None,
        })
        .collect();
    for c in hoisted {
        let id = file.add_decl(Decl::Class(c));
        file.decls.push(id);
    }
}

/// A class with NO primary constructor names its base class WITHOUT parentheses (`class A : B { …
/// constructor(): super(…) }`) — the base arguments come from each secondary `super(…)`, not a
/// `: Base(args)` supertype entry. The parser can't tell a parenless class supertype from an interface
/// syntactically, so it parks every parenless supertype in `supertypes`; here, with the whole file
/// visible, we move a supertype that names a concrete file class into `base_class` for such classes.
fn fixup_parenless_base_classes(file: &mut File) {
    use crate::ast::{CtorDelegation, Decl};
    let base_candidates: std::collections::HashSet<String> = file
        .decl_arena
        .iter()
        .filter_map(|d| match d {
            Decl::Class(c) if c.kind == ClassKind::Class || c.is_annotation() => {
                Some(c.name.clone())
            }
            _ => None,
        })
        .collect();
    for d in file.decl_arena.iter_mut() {
        if let Decl::Class(c) = d {
            if c.has_primary_ctor || c.base_class.is_some() {
                continue;
            }
            let super_delegates = c.secondary_ctors.iter().any(|sc| {
                matches!(
                    sc.delegation,
                    CtorDelegation::Super(_) | CtorDelegation::None
                )
            });
            if !super_delegates {
                continue;
            }
            if let Some(pos) = c
                .supertypes
                .iter()
                .position(|s| base_candidates.contains(s))
            {
                c.base_class = Some(c.supertypes.remove(pos));
            }
        }
    }
}

struct Parser<'a> {
    src: &'a str,
    t: &'a [Token],
    i: usize,
    file: File,
    diags: &'a mut DiagSink,
    /// `NameBasedDestructuring` language feature: allow square-bracket destructuring (`[a, b]`).
    name_based_destructuring: bool,
    /// When set, `parse_postfix` does NOT attach a trailing `{ … }` to a call as a lambda argument —
    /// used where a following `{` belongs to an enclosing construct (a `: I by Impl()` delegate, whose
    /// `{` opens the class body, not a lambda on the delegate call).
    no_trailing_lambda: bool,
    /// Type parameters in the current lexical parser context. Synthetic anonymous classes are hoisted
    /// to file-level declarations, so they must carry the generic names they mention in supertypes or
    /// member signatures; otherwise checking the hoisted class reports `T` as unresolved.
    lexical_type_params: Vec<String>,
    lexical_type_param_bounds: Vec<(String, TypeRef)>,
    /// Simple names of annotations consumed by the most recent `skip_decl_prefix`, awaiting attachment
    /// to the declaration that follows (e.g. `@Serializable` → `["Serializable"]`). A `parse_X` reads
    /// it via `take_pending_annotations()` *before* parsing members (member prefixes overwrite it).
    pending_annotations: Vec<String>,
    /// The argument expressions of each pending annotation (parallel to `pending_annotations`). Only the
    /// direct, ordinary-expression args are kept (array/nested-annotation args record an empty vec), which
    /// is all an extension reading a value annotation (`@SerialName("x")`) needs.
    pending_annotation_args: Vec<Vec<ExprId>>,
}

impl<'a> Parser<'a> {
    fn push_lexical_type_params(
        &mut self,
        params: &[String],
        bounds: &[(String, TypeRef)],
    ) -> (usize, usize) {
        let old_params_len = self.lexical_type_params.len();
        let old_bounds_len = self.lexical_type_param_bounds.len();
        self.lexical_type_params.extend(params.iter().cloned());
        self.lexical_type_param_bounds
            .extend(bounds.iter().cloned());
        (old_params_len, old_bounds_len)
    }

    fn pop_lexical_type_params(&mut self, old_lens: (usize, usize)) {
        self.lexical_type_params.truncate(old_lens.0);
        self.lexical_type_param_bounds.truncate(old_lens.1);
    }

    fn current_lexical_type_params(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in &self.lexical_type_params {
            if !out.iter().any(|existing| existing == p) {
                out.push(p.clone());
            }
        }
        out
    }

    fn current_lexical_type_param_bounds(&self) -> Vec<(String, TypeRef)> {
        let params = self.current_lexical_type_params();
        self.lexical_type_param_bounds
            .iter()
            .filter(|(name, _)| params.iter().any(|p| p == name))
            .cloned()
            .collect()
    }

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
            self.diags
                .error(self.tok().span, format!("expected {what}"));
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
            let mods = if self.at(TokenKind::At)
                || (self.at(TokenKind::Ident) && is_modifier(self.text()))
            {
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
                    let mut fq = self.parse_qualified_name();
                    // `import a.b.*` — `parse_qualified_name` consumes the trailing `.` (it only keeps a
                    // segment when an `Ident` follows), leaving us at `*`. Recover the wildcard so it is
                    // recorded as `a.b.*` (the form `import_wildcards` recognizes).
                    if self.at(TokenKind::Star) {
                        self.bump(); // '*'
                        fq.push_str(".*");
                    }
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
                    if self.t.get(self.i + 1).map_or(false, |t| {
                        t.kind == TokenKind::Ident && t.text(self.src) == "interface"
                    }) =>
                {
                    self.bump(); // 'fun'
                    let mut d = self.parse_interface();
                    d.is_fun_interface = true;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                TokenKind::KwFun => {
                    let mut d = self.parse_fun(
                        mods.iter().any(|m| m == "inline"),
                        mods.iter().any(|m| m == "final"),
                        mods.iter().any(|m| m == "suspend"),
                        mods.iter().any(|m| m == "tailrec"),
                    );
                    d.is_private = mods.iter().any(|m| m == "private");
                    let id = self.file.add_decl(Decl::Fun(d));
                    self.file.decls.push(id);
                }
                TokenKind::KwClass => {
                    let is_value = mods.iter().any(|m| m == "inline" || m == "value");
                    let mut d = self.parse_class();
                    d.modality = modality_of(is_open, is_abstract, is_sealed);
                    d.is_value = is_value;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // top-level property: `val`/`var name (: Type)? = init`
                TokenKind::KwVal | TokenKind::KwVar => {
                    let d = self.parse_top_property_c(
                        mods.iter().any(|m| m == "lateinit"),
                        false,
                        mods.iter().any(|m| m == "const"),
                        false,
                    );
                    let id = self.file.add_decl(Decl::Property(d));
                    self.file.decls.push(id);
                }
                // `data class` / `data object` — `data` is a soft keyword (a plain identifier elsewhere).
                TokenKind::Ident
                    if self.text() == "data"
                        && self.t.get(self.i + 1).map_or(false, |t| {
                            t.kind == TokenKind::KwClass
                                || (t.kind == TokenKind::Ident && t.text(self.src) == "object")
                        }) =>
                {
                    self.bump(); // 'data'
                    let is_obj = self.at(TokenKind::Ident) && self.text() == "object";
                    let mut d = if is_obj {
                        self.parse_object()
                    } else {
                        self.parse_class()
                    };
                    d.is_data = true;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `object Name { … }` — a singleton (soft keyword `object` + a name).
                TokenKind::Ident
                    if self.text() == "object"
                        && self
                            .t
                            .get(self.i + 1)
                            .map_or(false, |t| t.kind == TokenKind::Ident) =>
                {
                    let d = self.parse_object();
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `annotation class Name(...)` — emitted as an interface extending
                // `java/lang/annotation/Annotation` with an accessor per primary-ctor property;
                // instantiation synthesizes an impl class (see emit).
                TokenKind::Ident
                    if self.text() == "annotation"
                        && self
                            .t
                            .get(self.i + 1)
                            .map_or(false, |t| t.kind == TokenKind::KwClass) =>
                {
                    self.bump(); // 'annotation'
                    let mut d = self.parse_class();
                    d.kind = ClassKind::Annotation;
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `enum class Name { A, B, C }` (soft keyword `enum` + `class`).
                TokenKind::Ident
                    if self.text() == "enum"
                        && self
                            .t
                            .get(self.i + 1)
                            .map_or(false, |t| t.kind == TokenKind::KwClass) =>
                {
                    let d = self.parse_enum();
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `interface Name { … }` (soft keyword `interface` + a name). A `sealed interface` carries
                // `is_sealed` so it serializes as a `SealedClassSerializer` (closed polymorphism), like a
                // `sealed class` — not the open `PolymorphicSerializer` a plain interface gets.
                TokenKind::Ident
                    if self.text() == "interface"
                        && self
                            .t
                            .get(self.i + 1)
                            .map_or(false, |t| t.kind == TokenKind::Ident) =>
                {
                    let mut d = self.parse_interface();
                    if is_sealed {
                        d.modality = crate::ast::Modality::Sealed;
                    }
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.push(id);
                }
                // `typealias Name[<T,...>] = Type`
                TokenKind::Ident if self.text() == "typealias" => {
                    self.bump(); // `typealias`
                    let alias = if self.at(TokenKind::Ident) {
                        self.bump().text(self.src).to_string()
                    } else {
                        String::new()
                    };
                    self.parse_type_args(); // skip `<T, R>` if present
                    self.eat(TokenKind::Eq);
                    // Parse the target type name, including dotted FQNs (e.g. java.lang.Exception).
                    let target = if self.at(TokenKind::LParen) {
                        // function type — skip entire line
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
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
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                        name
                    } else {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                        String::new()
                    };
                    if !alias.is_empty() && !target.is_empty() {
                        self.file.type_aliases.push((alias, target));
                    }
                }
                _ => {
                    self.diags
                        .error(self.tok().span, "expected a top-level declaration");
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
        self.pending_annotations.clear();
        self.pending_annotation_args.clear();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::At) {
                let (name, args) = self.skip_annotation();
                if let Some(name) = name {
                    self.pending_annotations.push(name);
                    self.pending_annotation_args.push(args);
                }
            } else if self.at(TokenKind::Ident) && is_modifier(self.text()) {
                mods.push(self.text().to_string());
                self.bump();
            } else {
                break;
            }
        }
        mods
    }

    /// Take the annotations captured by the preceding `skip_decl_prefix`, clearing the buffer.
    /// `parse_class`/`parse_enum`/… call this FIRST so member-prefix parsing doesn't clobber them.
    fn take_pending_annotations(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_annotations)
    }

    /// Take the per-annotation argument expressions captured by the preceding `skip_decl_prefix`
    /// (parallel to [`take_pending_annotations`]), clearing the buffer.
    fn take_pending_annotation_args(&mut self) -> Vec<Vec<ExprId>> {
        std::mem::take(&mut self.pending_annotation_args)
    }

    /// Parse a nested type declaration (`class`/`object`/`interface`/`data|enum|annotation class`/
    /// `sealed …`) through the *real* parsers — never by skipping a balanced body. The current
    /// `class`-body/`object`-body/`enum`-body grammar doesn't support nested types, so the caller
    /// discards the result; a *reference* to the (dropped) nested type then fails to resolve and the
    /// file is cleanly skipped, never miscompiled.
    /// Whether the statement at the cursor begins a local TYPE declaration: a `class` keyword, an
    /// `interface Name`, or a soft-keyword class form (`data`/`enum`/`sealed`/`annotation`/`value` +
    /// `class`, possibly through modifiers like `open`/`abstract`/`inner`/`private`). Lookahead only —
    /// doesn't consume. Excludes `object` (a bare `object` may be an anonymous-object expression).
    fn looks_like_local_type_decl(&self) -> bool {
        let mut j = self.i;
        loop {
            let Some(tk) = self.t.get(j) else {
                return false;
            };
            if tk.kind == TokenKind::KwClass {
                return true;
            }
            if tk.kind != TokenKind::Ident {
                return false;
            }
            let s = tk.text(self.src);
            // `interface Name` — a named local interface (the next token is the name).
            if s == "interface" {
                return matches!(self.t.get(j + 1), Some(n) if n.kind == TokenKind::Ident);
            }
            // `object Name` — a named local object DECLARATION. A bare `object :`/`object {` is an
            // anonymous-object EXPRESSION (no name), which stays on the expression path.
            if s == "object" {
                return matches!(self.t.get(j + 1), Some(n) if n.kind == TokenKind::Ident);
            }
            // A class-introducing soft keyword or a declaration modifier (`open`/`abstract`/`private`/
            // `inner`/…) — keep scanning toward `class`/`interface`. The scan only returns `true` if it
            // actually reaches a type keyword, so a soft-keyword used as a value (`data.x`, `value.foo()`)
            // doesn't misfire.
            if matches!(s, "data" | "enum" | "sealed" | "annotation" | "value") || is_modifier(s) {
                j += 1;
                continue;
            }
            return false;
        }
    }

    fn parse_nested_type_decl(&mut self) -> ClassDecl {
        match self.kind() {
            TokenKind::KwClass => self.parse_class(),
            TokenKind::Ident if self.text() == "object" => self.parse_object(),
            TokenKind::Ident if self.text() == "interface" => self.parse_interface(),
            TokenKind::Ident if self.text() == "enum" => self.parse_enum(),
            TokenKind::Ident if self.text() == "data" => {
                self.bump();
                let mut d = if self.at(TokenKind::Ident) && self.text() == "object" {
                    self.parse_object()
                } else {
                    self.parse_class()
                };
                d.is_data = true;
                d
            }
            TokenKind::Ident if self.text() == "annotation" => {
                self.bump();
                self.parse_class()
            }
            TokenKind::Ident if self.text() == "sealed" => {
                self.bump();
                self.parse_nested_type_decl()
            }
            _ => self.parse_class(),
        }
    }

    /// Consume one `@Foo(...)` annotation; returns its **simple name** (last path segment) so a plugin
    /// can match it (`@kotlinx.serialization.Serializable` → `"Serializable"`). `None` for a use-site
    /// `@file:`/`@get:`… target annotation, which doesn't apply to the declaration.
    fn skip_annotation(&mut self) -> (Option<String>, Vec<ExprId>) {
        self.bump(); // '@'
                     // optional use-site target: `file:`, `get:`, `param:`, ...
        let mut use_site = false;
        let mut target = String::new();
        if self.at(TokenKind::Ident)
            && self
                .t
                .get(self.i + 1)
                .map_or(false, |t| t.kind == TokenKind::Colon)
        {
            target = self.text().to_string();
            self.bump();
            self.bump(); // ':'
            use_site = true;
        }
        let qname = self.parse_qualified_name();
        self.parse_type_args(); // `@Foo<Bar>` (rare) — real type-arg parse
        let args = self.parse_annotation_args();
        // A `@file:Foo(args)` annotation applies to the file, not the next declaration — record it for
        // plugins (e.g. `@file:UseContextualSerialization(MyDate::class)`) rather than dropping it.
        if target == "file" && !qname.is_empty() {
            let simple = qname.rsplit('.').next().unwrap_or(&qname).to_string();
            self.file.file_annotations.push((simple, args.clone()));
        }
        if use_site || qname.is_empty() {
            (None, args)
        } else {
            (
                Some(qname.rsplit('.').next().unwrap_or(&qname).to_string()),
                args,
            )
        }
    }

    /// Parse an annotation argument list `( (name =)? value ,* )` through the real grammar, returning
    /// the ordinary-expression arguments (array/nested-annotation values contribute nothing). The exprs
    /// are real AST nodes so an extension can const-fold a value (`@SerialName("$prefix.bar")`).
    fn parse_annotation_args(&mut self) -> Vec<ExprId> {
        let mut out = Vec::new();
        if !self.eat(TokenKind::LParen) {
            return out;
        }
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            // optional named argument `name = value`
            if self.at(TokenKind::Ident)
                && self
                    .t
                    .get(self.i + 1)
                    .map_or(false, |t| t.kind == TokenKind::Eq)
            {
                self.bump(); // name
                self.bump(); // '='
            }
            if let Some(e) = self.parse_annotation_value() {
                out.push(e);
            }
            self.skip_newlines();
            if !self.eat(TokenKind::Comma) {
                break;
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RParen, "')'");
        out
    }

    /// A single annotation argument value: an array literal `[…]`, a nested annotation `@Foo(…)`,
    /// or an ordinary expression (incl. `Foo::class`). Returns the expr for the ordinary case (kept for
    /// const-folding by extensions); array/nested values return `None`.
    fn parse_annotation_value(&mut self) -> Option<ExprId> {
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
            None
        } else if self.at(TokenKind::At) {
            self.skip_annotation();
            None
        } else {
            Some(self.parse_expr())
        }
    }

    /// `abstract_ok` — allow missing initializer (abstract/interface props, class/object body props
    /// with init blocks, etc.). Top-level properties always require an initializer.
    fn parse_top_property(&mut self, is_lateinit: bool, abstract_ok: bool) -> PropDecl {
        self.parse_top_property_c(is_lateinit, abstract_ok, false, false)
    }

    fn parse_top_property_c(
        &mut self,
        is_lateinit: bool,
        abstract_ok: bool,
        is_const: bool,
        is_abstract: bool,
    ) -> PropDecl {
        let start = self.tok().span;
        let is_var = self.at(TokenKind::KwVar);
        self.bump(); // val/var
                     // Optional generic type parameters on an extension property (`val <T> T.foo: T`) — erased.
        if self.at(TokenKind::Lt) {
            self.parse_type_params();
        }
        let first = self.ident_or_error("property name");
        // Optional extension receiver: `val Recv[<…>][?].name` (like an extension function).
        let (receiver, name) =
            if self.at(TokenKind::Dot) || self.at(TokenKind::Lt) || self.at(TokenKind::Question) {
                let span = self.tok().span;
                if self.at(TokenKind::Lt) {
                    self.parse_type_args(); // type args on the receiver — erased
                }
                let nullable = self.eat_type_nullable();
                self.expect(TokenKind::Dot, "'.'");
                let recv = TypeRef {
                    name: first,
                    nullable,
                    arg: None,
                    targs: vec![],
                    span,
                    fun_params: vec![],
                    fun_has_receiver: false,
                    fun_suspend: false,
                };
                (Some(recv), self.ident_or_error("property name"))
            } else {
                (None, first)
            };
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let init = if self.eat(TokenKind::Eq) {
            self.skip_newlines();
            Some(self.parse_expr())
        } else {
            None
        };
        // `val x: T by <expr>` — a delegated property (in place of `= init`). Reads/writes route through
        // the delegate's `getValue`/`setValue` operators.
        let delegate = if init.is_none() && self.at(TokenKind::Ident) && self.text() == "by" {
            self.bump(); // 'by'
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
            if self.at(TokenKind::Ident)
                && matches!(self.text(), "private" | "protected" | "internal" | "public")
            {
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
                setter = Some(PropAccessor {
                    param,
                    body,
                    is_private,
                });
            }
        }
        // A property with no initializer, no getter, and no backing-field need must be `lateinit`
        // (or an abstract/interface property); an extension property always has a getter, so it is
        // exempt.
        if init.is_none()
            && delegate.is_none()
            && getter.is_none()
            && setter.is_none()
            && !is_lateinit
            && !abstract_ok
            && !is_abstract
            && receiver.is_none()
        {
            self.diags.error(
                start,
                "krusty: a property without an initializer must be 'lateinit'",
            );
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        PropDecl {
            name,
            receiver,
            ty,
            is_var,
            init,
            is_lateinit,
            getter,
            setter,
            is_const,
            is_abstract,
            delegate,
            span: Span::new(start.lo, end.hi),
        }
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
            self.diags.error(
                self.tok().span,
                "expected '=' or '{' for a property getter".to_string(),
            );
            FunBody::None
        }
    }

    /// `companion object [Name] [: Super] { fun…; val… }` — collect its functions/properties to be
    /// emitted as `static`/`static final` members of the enclosing class.
    fn parse_companion(
        &mut self,
        methods: &mut Vec<FunDecl>,
        props: &mut Vec<PropDecl>,
        base: &mut Option<String>,
        base_args: &mut Vec<ExprId>,
        supertypes: &mut Vec<String>,
    ) {
        self.bump(); // 'companion'
        self.bump(); // 'object'
        if self.at(TokenKind::Ident) {
            self.bump(); // optional companion name
        }
        // Capture the companion's supertype list (`companion object : Base(args), I`): the synthesized
        // `C$Companion` extends `Base` (ctor `super(args)`) and implements the interfaces, so the
        // companion can be used as a value of that supertype (e.g. `EmptyContinuation` as a `Continuation`).
        let (ifaces, b, b_args, _delegations, _expr_delegations) = self.parse_supertypes();
        *base = b;
        *base_args = b_args;
        *supertypes = ifaces;
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
                    let mut d = self.parse_fun(
                        mods.iter().any(|m| m == "inline"),
                        mods.iter().any(|m| m == "final"),
                        mods.iter().any(|m| m == "suspend"),
                        mods.iter().any(|m| m == "tailrec"),
                    );
                    d.is_private = mods.iter().any(|m| m == "private");
                    methods.push(d);
                }
                TokenKind::KwVal | TokenKind::KwVar => props.push(self.parse_top_property_c(
                    lateinit,
                    false,
                    mods.iter().any(|m| m == "const"),
                    false,
                )),
                _ => {
                    self.diags.error(
                        self.tok().span,
                        "krusty: companion bodies support only 'fun' and 'val'/'var'",
                    );
                    self.bump();
                }
            }
        }
        self.expect(TokenKind::RBrace, "'}'");
    }

    /// `enum class Name { A, B, C }` — v0: simple entries (no constructor args, no class body).
    fn parse_enum(&mut self) -> ClassDecl {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
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
                props.push(PropParam {
                    name: pname,
                    ty,
                    is_var,
                    is_property,
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
        }
        // Optional supertype list (`enum class E : I1, I2`): an enum may implement interfaces; the
        // abstract members are satisfied by the enum's own methods or per-entry overrides. (An enum can't
        // extend a class, so only the interface supertypes are kept.)
        let enum_supertypes = if self.at(TokenKind::Colon) {
            let (supertypes, _base, _args, _del, _del_e) = self.parse_supertypes();
            supertypes
        } else {
            Vec::new()
        };
        let mut entries: Vec<AstEnumEntry> = Vec::new();
        let mut methods = Vec::new();
        self.skip_newlines();
        if self.eat(TokenKind::LBrace) {
            self.skip_newlines();
            while self.at(TokenKind::Ident) {
                let entry_name = self.text().to_string();
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
                // Capture its method overrides and `val`/`var` properties; anything else bails cleanly.
                let mut body = Vec::new();
                let mut bprops = Vec::new();
                if self.eat(TokenKind::LBrace) {
                    self.skip_newlines();
                    while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                        let bmods = if self.at(TokenKind::At)
                            || (self.at(TokenKind::Ident) && is_modifier(self.text()))
                        {
                            let m = self.skip_decl_prefix();
                            self.skip_newlines();
                            m
                        } else {
                            Vec::new()
                        };
                        if self.at(TokenKind::KwFun) {
                            body.push(self.parse_fun(
                                bmods.iter().any(|m| m == "inline"),
                                bmods.iter().any(|m| m == "final"),
                                bmods.iter().any(|m| m == "suspend"),
                                bmods.iter().any(|m| m == "tailrec"),
                            ));
                        } else if self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar) {
                            bprops.push(
                                self.parse_top_property(
                                    bmods.iter().any(|m| m == "lateinit"),
                                    false,
                                ),
                            );
                        } else {
                            self.diags.error(
                                self.tok().span,
                                "krusty: only methods and properties are supported in an enum entry body",
                            );
                            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                                self.bump();
                            }
                        }
                        self.skip_newlines();
                    }
                    self.expect(TokenKind::RBrace, "'}'");
                }
                entries.push(AstEnumEntry {
                    name: entry_name,
                    args,
                    methods: body,
                    props: bprops,
                });
                self.skip_newlines();
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
            // Members follow a `;` separator (lexed as a newline): `enum class C { A, B; fun f() … }`.
            loop {
                self.skip_newlines();
                let emods = if self.at(TokenKind::At)
                    || (self.at(TokenKind::Ident) && is_modifier(self.text()))
                {
                    let m = self.skip_decl_prefix();
                    self.skip_newlines();
                    m
                } else {
                    Vec::new()
                };
                match self.kind() {
                    TokenKind::KwFun => methods.push(self.parse_fun(
                        emods.iter().any(|m| m == "inline"),
                        emods.iter().any(|m| m == "final"),
                        emods.iter().any(|m| m == "suspend"),
                        emods.iter().any(|m| m == "tailrec"),
                    )),
                    // Nested type declarations and secondary constructors in an enum body: parse
                    // them through the real grammar (no token-skipping) and discard — krusty doesn't
                    // emit them, so a reference fails to resolve and the file is cleanly skipped.
                    TokenKind::KwClass => {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "constructor" => {
                        self.diags.error(
                            self.tok().span,
                            "krusty: secondary constructors in enum classes are not supported",
                        );
                        self.bump(); // 'constructor'
                        let _ = self.parse_param_list();
                        if self.eat(TokenKind::Colon) {
                            self.skip_newlines();
                            if self.at(TokenKind::Ident) {
                                self.bump();
                            } // 'this'/'super'
                            let _ = self.parse_call_arguments();
                        }
                        self.skip_newlines();
                        if self.at(TokenKind::LBrace) {
                            let _ = self.parse_block_expr();
                        }
                    }
                    TokenKind::Ident if matches!(self.text(), "object" | "interface") => {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "companion" => {
                        self.bump();
                        let _ = self.parse_nested_type_decl();
                    }
                    _ => break,
                }
            }
            self.skip_newlines();
            if !self.at(TokenKind::RBrace) {
                self.diags
                    .error(self.tok().span, "krusty: unsupported enum member");
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl {
            name,
            annotations,
            annotation_args,
            type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            props,
            methods,
            companion_methods: Vec::new(),
            companion_props: Vec::new(),
            companion_base: None,
            companion_base_args: Vec::new(),
            companion_supertypes: Vec::new(),
            body_props: Vec::new(),
            init_order: Vec::new(),
            is_data: false,
            is_value: false,
            kind: ClassKind::Enum,
            enum_entries: entries,
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes: enum_supertypes,
            delegations: Vec::new(),
            delegation_exprs: Vec::new(),
            base_class: None,
            base_args: Vec::new(),
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
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
                if crate::types::Ty::from_name(&bound.name).is_some_and(|t| {
                    matches!(
                        t,
                        crate::types::Ty::Int
                            | crate::types::Ty::Byte
                            | crate::types::Ty::Short
                            | crate::types::Ty::Long
                            | crate::types::Ty::Float
                            | crate::types::Ty::Double
                            | crate::types::Ty::Boolean
                            | crate::types::Ty::Char
                            | crate::types::Ty::UInt
                            | crate::types::Ty::ULong
                    )
                }) {
                    self.diags.error(
                        bound.span,
                        "krusty: type parameter with a primitive upper bound is not supported"
                            .to_string(),
                    );
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

    fn parse_fun(
        &mut self,
        is_inline: bool,
        is_final: bool,
        is_suspend: bool,
        is_tailrec: bool,
    ) -> FunDecl {
        // Annotations consumed by `skip_decl_prefix` before this function, attached here (mirrors how
        // classes take them) so function-annotation plugins can see them; otherwise they are discarded.
        let annotations = self.take_pending_annotations();
        let _ = self.take_pending_annotation_args(); // a function decl doesn't carry annotation args yet
        let start = self.tok().span;
        self.bump(); // 'fun'
                     // `fun interface` is a SAM/functional interface declaration — not a regular function.
                     // Skip the entire interface body with a clean unsupported-feature message.
        if self.at(TokenKind::Ident) && self.text() == "interface" {
            self.diags.error(
                start,
                "krusty: 'fun interface' (SAM interfaces) are not supported",
            );
            self.bump(); // 'interface'
            if self.at(TokenKind::Ident) {
                self.bump();
            } // interface name
            self.parse_type_args();
            let (supertypes, _, _, _, _) = self.parse_supertypes();
            let _ = supertypes;
            if self.at(TokenKind::LBrace) {
                let _ = self.parse_block_expr();
            }
            return FunDecl {
                name: "<fun-interface>".to_string(),
                receiver: None,
                params: vec![],
                ret: None,
                body: FunBody::None,
                type_params: vec![],
                type_param_bounds: Vec::new(),
                non_null_type_params: Default::default(),
                reified_type_params: Default::default(),
                span: start,
                is_inline: false,
                is_final: false,
                is_private: false,
                is_suspend: false,
                is_tailrec: false,
                annotations,
            };
        }
        let (type_params, non_null_type_params, reified_type_params, type_param_bounds) =
            if self.at(TokenKind::Lt) {
                self.parse_type_params()
            } else {
                (
                    Vec::new(),
                    std::collections::HashSet::new(),
                    std::collections::HashSet::new(),
                    Vec::new(),
                )
            };
        let lexical_type_param_lens =
            self.push_lexical_type_params(&type_params, &type_param_bounds);
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
        let (receiver, name) =
            if self.at(TokenKind::Dot) || self.at(TokenKind::Lt) || self.at(TokenKind::Question) {
                // `fun RecvType<...>?.name(...)` — extension function.
                let span = self.tok().span;
                let mut recv_nullable = false;
                if self.at(TokenKind::Lt) {
                    self.parse_type_args();
                } // skip type args on receiver
                if self.eat(TokenKind::Question) {
                    recv_nullable = true;
                }
                self.expect(TokenKind::Dot, "'.'");
                // The receiver type may be DOTTED (`fun Int.Companion.MAX()`, `fun Foo.Bar.baz()`):
                // consume `Ident` segments while each is followed by another `.`; the final segment (the
                // one NOT followed by a `.`) is the function name, the rest form the receiver type name.
                let mut recv_name = first_name;
                let mut fun_name = "<error>".to_string();
                loop {
                    let seg = if self.at(TokenKind::Ident) {
                        let n = self.text().to_string();
                        self.bump();
                        n
                    } else {
                        self.diags
                            .error(self.tok().span, "expected extension function name");
                        break;
                    };
                    if self.eat(TokenKind::Dot) {
                        recv_name.push('.');
                        recv_name.push_str(&seg);
                    } else {
                        fun_name = seg;
                        break;
                    }
                }
                let recv_ty = TypeRef {
                    name: recv_name,
                    nullable: recv_nullable,
                    arg: None,
                    targs: vec![],
                    span,
                    fun_params: vec![],
                    fun_has_receiver: false,
                    fun_suspend: false,
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
        self.pop_lexical_type_params(lexical_type_param_lens);
        FunDecl {
            name,
            receiver,
            params,
            ret,
            body,
            type_params,
            type_param_bounds,
            non_null_type_params,
            reified_type_params,
            span: Span::new(start.lo, end.hi),
            is_inline,
            is_final,
            is_private: false,
            is_suspend,
            is_tailrec,
            annotations,
        }
    }

    /// Parse a parenthesised parameter list `( (mods name: Type (= default)?),* )` via the real
    /// grammar — never by skipping to a balanced `)`.
    fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.expect(TokenKind::LParen, "'('");
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let mut pmods = Vec::new();
            let mut pannos = Vec::new();
            let mut pannos_args = Vec::new();
            // `value` is a valid parameter name in Kotlin; only collect real parameter modifiers.
            if self.at(TokenKind::At)
                || (self.at(TokenKind::Ident) && is_modifier(self.text()) && self.text() != "value")
            {
                pmods = self.skip_decl_prefix(); // `@Anno`, `vararg`, `noinline`, … on a parameter
                pannos = self.take_pending_annotations();
                pannos_args = self.take_pending_annotation_args();
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
            params.push(Param {
                name: pname,
                ty,
                is_vararg,
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
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
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
        let (type_params, _, _, type_param_bounds) = if self.at(TokenKind::Lt) {
            self.parse_type_params()
        } else {
            (
                Vec::new(),
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
                Vec::new(),
            )
        };
        let lexical_type_param_lens =
            self.push_lexical_type_params(&type_params, &type_param_bounds);
        // An explicit primary-constructor `constructor` keyword (`class A private constructor(...)`,
        // possibly preceded by modifiers/annotations) marks a primary ctor even before the params.
        if (self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())))
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::Ident && t.text(self.src) == "constructor")
        {
            self.skip_decl_prefix();
        }
        let header_ctor_kw = self.at(TokenKind::Ident) && self.text() == "constructor";
        if header_ctor_kw {
            self.bump();
        }
        let mut props = Vec::new();
        let has_primary_ctor_parens = self.eat(TokenKind::LParen);
        let header_has_primary = header_ctor_kw || has_primary_ctor_parens;
        if has_primary_ctor_parens {
            self.skip_newlines();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                let mut pannos = Vec::new();
                let mut pannos_args = Vec::new();
                if self.at(TokenKind::At)
                    || (self.at(TokenKind::Ident)
                        && is_modifier(self.text())
                        && self.text() != "value")
                {
                    self.skip_decl_prefix(); // `private val x`, `@Anno val y`, ...
                    pannos = self.take_pending_annotations();
                    pannos_args = self.take_pending_annotation_args();
                }
                let (is_property, is_var) = match self.kind() {
                    TokenKind::KwVal => {
                        self.bump();
                        (true, false)
                    }
                    TokenKind::KwVar => {
                        self.bump();
                        (true, true)
                    }
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
                props.push(PropParam {
                    name: pname,
                    ty,
                    is_var,
                    is_property,
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
        }
        // Optional supertype list: `: Iface1, Base(args), Iface2`. Supertypes with `()` are the
        // base class (v0: unsupported → flagged); the rest are implemented interfaces.
        let (supertypes, base_class, base_args, delegations, delegation_exprs) =
            self.parse_supertypes();
        // `class Derived<T> : Base<T>() where T : I1, T : I2` — generic constraints after the
        // supertype list, before the body.
        self.parse_where_clause();
        // Optional class body: member `fun`s, body properties (`val`/`var`), and `init { }` blocks.
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        let mut companion_methods: Vec<FunDecl> = Vec::new();
        let mut companion_props: Vec<PropDecl> = Vec::new();
        let mut companion_base: Option<String> = None;
        let mut companion_base_args: Vec<ExprId> = Vec::new();
        let mut companion_supertypes: Vec<String> = Vec::new();
        let mut secondary_ctors: Vec<SecondaryCtor> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let mut mods = Vec::new();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text()))
                {
                    mods = self.skip_decl_prefix();
                    self.skip_newlines();
                }
                let lateinit = mods.iter().any(|m| m == "lateinit");
                let fun_inline = mods.iter().any(|m| m == "inline");
                let fun_final = mods.iter().any(|m| m == "final");
                let fun_suspend = mods.iter().any(|m| m == "suspend");
                let is_abstract = mods.iter().any(|m| m == "abstract");
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun(
                        fun_inline,
                        fun_final,
                        fun_suspend,
                        mods.iter().any(|m| m == "tailrec"),
                    )),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        // Non-abstract body props may omit the initializer (init blocks supply the
                        // value); an `abstract` property has no field and is marked accordingly.
                        let p = self.parse_top_property_c(
                            lateinit,
                            !is_abstract,
                            mods.iter().any(|m| m == "const"),
                            is_abstract,
                        );
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident
                        if self.text() == "init"
                            && self
                                .t
                                .get(self.i + 1)
                                .map_or(false, |t| t.kind == TokenKind::LBrace) =>
                    {
                        self.bump(); // 'init'
                        let block = self.parse_block_expr();
                        init_order.push(ClassInit::Block(block));
                    }
                    // `companion object [Name] { fun…; val… }` — members become static on this class.
                    TokenKind::Ident
                        if self.text() == "companion"
                            && self.t.get(self.i + 1).map_or(false, |t| {
                                t.kind == TokenKind::Ident && t.text(self.src) == "object"
                            }) =>
                    {
                        self.parse_companion(
                            &mut companion_methods,
                            &mut companion_props,
                            &mut companion_base,
                            &mut companion_base_args,
                            &mut companion_supertypes,
                        );
                    }
                    // Silently skip nested type declarations (inner/nested class, object,
                    // interface, typealias) and secondary constructors.  Parsing them properly
                    // requires nesting the full resolver/emitter; for now we drop them and the
                    // file compiles so tests that don't exercise the nested type still pass.
                    TokenKind::KwClass => {
                        // A nested class `Outer { class Inner … }` is hoisted to the file top level as a
                        // separate class (internal `Outer$Inner`, source `Outer.Inner`). An `inner class`
                        // additionally captures the enclosing instance (`inner_of` → a synthetic `this$0`
                        // field + outer-as-first-constructor-parameter).
                        let is_inner = mods.iter().any(|m| m == "inner");
                        let mut nested = self.parse_class();
                        nested.name = format!("{}.{}", name, nested.name);
                        if is_inner {
                            nested.inner_of = Some(name.clone());
                        }
                        let id = self.file.add_decl(Decl::Class(nested));
                        self.file.decls.push(id);
                    }
                    // Nested `data class Inner(…)` → hoist like a plain nested class (`Outer.Inner`),
                    // constructed as `Outer.Inner(…)`; its data members emit normally.
                    TokenKind::Ident
                        if self.text() == "data"
                            && self
                                .t
                                .get(self.i + 1)
                                .map_or(false, |t| t.kind == TokenKind::KwClass) =>
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
                                && self
                                    .t
                                    .get(self.i + 1)
                                    .map_or(false, |t| t.kind == TokenKind::KwClass))
                            || (self.text() == "sealed"
                                && self.t.get(self.i + 1).map_or(false, |t| {
                                    t.kind == TokenKind::Ident && t.text(self.src) == "interface"
                                })) =>
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
                            let target = if self.at(TokenKind::Ident) {
                                let t = self.text().to_string();
                                self.bump();
                                t
                            } else {
                                String::new()
                            };
                            let args = self.parse_call_arguments();
                            delegation = match target.as_str() {
                                "this" => CtorDelegation::This(args),
                                "super" => CtorDelegation::Super(args),
                                _ => {
                                    self.diags.error(
                                        ctor_span,
                                        "expected 'this' or 'super' in constructor delegation",
                                    );
                                    CtorDelegation::None
                                }
                            };
                        }
                        self.skip_newlines();
                        let body = if self.at(TokenKind::LBrace) {
                            Some(self.parse_block_expr())
                        } else {
                            None
                        };
                        secondary_ctors.push(SecondaryCtor {
                            params,
                            delegation,
                            body,
                            span: ctor_span,
                        });
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                    }
                    _ => {
                        self.diags.error(
                            self.tok().span,
                            "v0: class bodies support member 'fun', 'val'/'var', and 'init' blocks",
                        );
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.pop_lexical_type_params(lexical_type_param_lens);
        ClassDecl {
            name,
            annotations,
            annotation_args,
            type_params,
            type_param_bounds,
            props,
            methods,
            companion_methods,
            companion_props,
            companion_base,
            companion_base_args,
            companion_supertypes,
            body_props,
            init_order,
            is_data: false,
            is_value: false,
            kind: ClassKind::Class,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations,
            delegation_exprs,
            base_class,
            base_args,
            // A class has a primary constructor when it wrote one (parens / `constructor` keyword) OR
            // declares no secondary constructors at all (then an implicit no-arg primary exists). Only a
            // class with secondary ctors and no header ctor has NO primary.
            has_primary_ctor: header_has_primary || secondary_ctors.is_empty(),
            secondary_ctors,
            span: Span::new(start.lo, end.hi),
        }
    }

    /// Parse an optional `: Base(args), Iface1, Iface2` supertype list. A supertype with `()` is the
    /// base class (returns its name + ctor-arg expressions); the rest are implemented interfaces.
    fn parse_supertypes(
        &mut self,
    ) -> (
        Vec<String>,
        Option<String>,
        Vec<ExprId>,
        Vec<(String, String)>,
        Vec<(String, ExprId)>,
    ) {
        let mut ifaces = Vec::new();
        let mut base: Option<String> = None;
        let mut base_args = Vec::new();
        let mut delegations = Vec::new();
        let mut delegation_exprs = Vec::new();
        if self.eat(TokenKind::Colon) {
            loop {
                self.skip_newlines();
                let name = self.parse_qualified_name();
                let simple = name.rsplit('.').next().unwrap_or(&name).to_string();
                // Fully-qualified name (e.g. java.util.RandomAccess) → JVM internal format.
                let effective = if name.contains('.') {
                    name.replace('.', "/")
                } else {
                    simple.clone()
                };
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
                        // A bare variable name (a `val`-param field) is the simple delegate form; any
                        // other shape (`by Impl()`, `by a.b`, …) is an EXPRESSION delegate.
                        if matches!(
                            after,
                            Some(TokenKind::Comma)
                                | Some(TokenKind::LBrace)
                                | Some(TokenKind::Newline)
                        ) {
                            self.bump();
                            delegations.push((effective.clone(), delegate));
                        } else {
                            // A following `{` opens the CLASS BODY, not a lambda on the delegate call.
                            let saved = self.no_trailing_lambda;
                            self.no_trailing_lambda = true;
                            let e = self.parse_expr();
                            self.no_trailing_lambda = saved;
                            delegation_exprs.push((effective.clone(), e));
                        }
                    } else {
                        let saved = self.no_trailing_lambda;
                        self.no_trailing_lambda = true;
                        let e = self.parse_expr();
                        self.no_trailing_lambda = saved;
                        delegation_exprs.push((effective.clone(), e));
                    }
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }
        (ifaces, base, base_args, delegations, delegation_exprs)
    }

    /// `interface Name { fun sig(): T }` — abstract member functions only (v0).
    fn parse_interface(&mut self) -> ClassDecl {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let start = self.tok().span;
        self.bump(); // 'interface'
        let name = self.ident_or_error("interface name");
        let (type_params, _, _, _) = if self.at(TokenKind::Lt) {
            self.parse_type_params()
        } else {
            (
                Vec::new(),
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
                Vec::new(),
            )
        };
        let (supertypes, _base, _base_args, _, _) = self.parse_supertypes();
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let imods = if self.at(TokenKind::At)
                    || (self.at(TokenKind::Ident) && is_modifier(self.text()))
                {
                    let m = self.skip_decl_prefix();
                    self.skip_newlines();
                    m
                } else {
                    Vec::new()
                };
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => {
                        let f = self.parse_fun(
                            imods.iter().any(|m| m == "inline"),
                            false,
                            imods.iter().any(|m| m == "suspend"),
                            imods.iter().any(|m| m == "tailrec"),
                        );
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
                    TokenKind::KwClass => {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(self.text(), "data" | "enum" | "annotation")
                                && self
                                    .t
                                    .get(self.i + 1)
                                    .map_or(false, |t| t.kind == TokenKind::KwClass)) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
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
            name,
            annotations,
            annotation_args,
            type_params,
            type_param_bounds: Vec::new(),
            props: Vec::new(),
            methods,
            companion_methods: Vec::new(),
            companion_props: Vec::new(),
            companion_base: None,
            companion_base_args: Vec::new(),
            companion_supertypes: Vec::new(),
            body_props,
            init_order: Vec::new(),
            is_data: false,
            is_value: false,
            kind: ClassKind::Interface,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations: Vec::new(),
            delegation_exprs: Vec::new(),
            base_class: None,
            base_args: Vec::new(),
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
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
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text()))
                {
                    mods = self.skip_decl_prefix();
                    self.skip_newlines();
                }
                let lateinit = mods.iter().any(|m| m == "lateinit");
                let fun_inline = mods.iter().any(|m| m == "inline");
                let fun_final = mods.iter().any(|m| m == "final");
                let fun_suspend = mods.iter().any(|m| m == "suspend");
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun(
                        fun_inline,
                        fun_final,
                        fun_suspend,
                        mods.iter().any(|m| m == "tailrec"),
                    )),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let p = self.parse_top_property_c(
                            lateinit,
                            true,
                            mods.iter().any(|m| m == "const"),
                            false,
                        );
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident
                        if self.text() == "init"
                            && self
                                .t
                                .get(self.i + 1)
                                .map_or(false, |t| t.kind == TokenKind::LBrace) =>
                    {
                        self.bump();
                        let block = self.parse_block_expr();
                        init_order.push(ClassInit::Block(block));
                    }
                    TokenKind::KwClass => {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(self.text(), "data" | "enum" | "annotation")
                                && self
                                    .t
                                    .get(self.i + 1)
                                    .map_or(false, |t| t.kind == TokenKind::KwClass)) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                    }
                    _ => {
                        self.diags.error(
                            self.tok().span,
                            "krusty: object bodies support 'fun', 'val'/'var', and 'init' blocks",
                        );
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
        let (supertypes, base_class, base_args, delegations, delegation_exprs) =
            self.parse_supertypes();
        let (methods, body_props, init_order) = self.parse_object_body();
        let end = self.t[self.i.saturating_sub(1)].span;
        let name = format!("Anon$anon${}", span.lo);
        let synth = ClassDecl {
            name: name.clone(),
            annotations: Vec::new(),
            annotation_args: Vec::new(),
            type_params: self.current_lexical_type_params(),
            type_param_bounds: self.current_lexical_type_param_bounds(),
            props: Vec::new(),
            methods,
            companion_methods: Vec::new(),
            companion_props: Vec::new(),
            companion_base: None,
            companion_base_args: Vec::new(),
            companion_supertypes: Vec::new(),
            body_props,
            init_order,
            is_data: false,
            is_value: false,
            kind: ClassKind::Class,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations,
            delegation_exprs,
            base_class,
            base_args,
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            span: Span::new(span.lo, end.hi),
        };
        let did = self.file.add_decl(Decl::Class(synth));
        self.file.decls.push(did);
        let callee = self.file.add_expr(Expr::Name(name), span);
        self.file.add_expr(
            Expr::Call {
                callee,
                args: Vec::new(),
            },
            Span::new(span.lo, end.hi),
        )
    }

    fn parse_object(&mut self) -> ClassDecl {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let start = self.tok().span;
        self.bump(); // 'object'
        let name = self.ident_or_error("object name");
        // Capture the object's implemented INTERFACES (`object X : KSerializer<C>`) AND a base class
        // (`object A : Sealed()`): the general class lowering/emit handles the `extends` + `super(args)`.
        let (supertypes, base_class, base_args, _delegations, _delegation_exprs) =
            self.parse_supertypes();
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let mut mods = Vec::new();
                if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text()))
                {
                    mods = self.skip_decl_prefix();
                    self.skip_newlines();
                }
                let lateinit = mods.iter().any(|m| m == "lateinit");
                let fun_inline = mods.iter().any(|m| m == "inline");
                let fun_final = mods.iter().any(|m| m == "final");
                let fun_suspend = mods.iter().any(|m| m == "suspend");
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => methods.push(self.parse_fun(
                        fun_inline,
                        fun_final,
                        fun_suspend,
                        mods.iter().any(|m| m == "tailrec"),
                    )),
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let p = self.parse_top_property_c(
                            lateinit,
                            true,
                            mods.iter().any(|m| m == "const"),
                            false,
                        ); // init blocks may supply the value
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident
                        if self.text() == "init"
                            && self
                                .t
                                .get(self.i + 1)
                                .map_or(false, |t| t.kind == TokenKind::LBrace) =>
                    {
                        self.bump();
                        let block = self.parse_block_expr();
                        init_order.push(ClassInit::Block(block));
                    }
                    TokenKind::KwClass => {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(self.text(), "data" | "enum" | "annotation")
                                && self
                                    .t
                                    .get(self.i + 1)
                                    .map_or(false, |t| t.kind == TokenKind::KwClass)) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                    }
                    _ => {
                        self.diags.error(
                            self.tok().span,
                            "krusty: object bodies support 'fun', 'val'/'var', and 'init' blocks",
                        );
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        ClassDecl {
            name,
            annotations,
            annotation_args,
            type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            props: Vec::new(),
            methods,
            companion_methods: Vec::new(),
            companion_props: Vec::new(),
            companion_base: None,
            companion_base_args: Vec::new(),
            companion_supertypes: Vec::new(),
            body_props,
            init_order,
            is_data: false,
            is_value: false,
            kind: ClassKind::Object,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations: Vec::new(),
            delegation_exprs: Vec::new(),
            base_class,
            base_args,
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            span: Span::new(start.lo, end.hi),
        }
    }

    /// Eat a `?` nullable-type marker, but NOT the `?` of an `?:` elvis. A nullable type is never
    /// validly followed by `:`, so a `?` immediately before `:` is the elvis operator (e.g.
    /// `x as? T ?: y` — the cast type is `T`, then `?: y`), which `parse_type` must leave for the caller.
    fn eat_type_nullable(&mut self) -> bool {
        if self.at(TokenKind::Question)
            && self
                .t
                .get(self.i + 1)
                .is_none_or(|t| t.kind != TokenKind::Colon)
        {
            self.bump();
            true
        } else {
            false
        }
    }

    fn parse_type(&mut self) -> TypeRef {
        // Leading type annotations (`@Composable () -> Unit`, `@UnsafeVariance T`): consume them and
        // record by the type's start offset so a plugin can recover them via `TypeRef.span.lo`.
        // Without this, an `@` before a type would fail to parse. NOTE: a following `(` is NOT consumed
        // as an annotation argument list here — in type position it belongs to a function type
        // (`@Composable () -> Unit`); an argument-bearing type annotation (`@Foo(1) Bar`, rare) is not
        // yet handled.
        let mut type_anns = Vec::new();
        while self.at(TokenKind::At) {
            self.bump(); // '@'
            let qname = self.parse_qualified_name();
            self.parse_type_args(); // `@Foo<Bar>` — type arguments on the annotation
            if !qname.is_empty() {
                type_anns.push(qname.rsplit('.').next().unwrap_or(&qname).to_string());
            }
        }
        let span = self.tok().span;
        if !type_anns.is_empty() {
            self.file.type_annotations.insert(span.lo, type_anns);
        }
        // `suspend` modifier on a function type: `suspend (A) -> B` — consume and parse as function type.
        let mut fun_suspend = false;
        if self.at(TokenKind::Ident) && self.text() == "suspend" {
            self.bump(); // 'suspend'
            fun_suspend = true;
        }
        // Function type: `(A, B) -> R` — starts with `(`.
        if self.at(TokenKind::LParen) {
            self.bump(); // '('
            let mut fun_params = Vec::new();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                // Skip optional parameter name prefix `name: Type` — consume up to a colon if present.
                // Peek ahead: if next two tokens are Ident + Colon, skip them.
                if self.at(TokenKind::Ident)
                    && self
                        .t
                        .get(self.i + 1)
                        .map_or(false, |t| t.kind == TokenKind::Colon)
                {
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
                let nullable = self.eat_type_nullable();
                TypeRef {
                    name: "<fun>".to_string(),
                    nullable,
                    arg: Some(Box::new(ret)),
                    targs: Vec::new(),
                    span,
                    fun_params,
                    fun_has_receiver: false,
                    fun_suspend,
                }
            } else if fun_params.len() == 1 && !fun_suspend {
                // A PARENTHESIZED type used for grouping (no `->` follows the `)`), most commonly to make
                // a function type nullable: `(() -> Unit)?` ≡ `Function0<Unit>?`. The parens wrap a single
                // type; an optional trailing `?` applies to it. (Kotlin permits redundant parens around any
                // type — `(Int)`, `(String)?`.)
                let mut inner = fun_params.into_iter().next().unwrap();
                if self.eat_type_nullable() {
                    inner.nullable = true;
                }
                inner
            } else {
                // Parenthesized multi-element type (a tuple) — krusty doesn't support tuple types.
                self.diags.error(span, "expected '->' for function type");
                TypeRef {
                    name: "<error>".to_string(),
                    nullable: false,
                    arg: None,
                    targs: Vec::new(),
                    span,
                    fun_params: Vec::new(),
                    fun_has_receiver: false,
                    fun_suspend: false,
                }
            }
        } else if self.at(TokenKind::Ident) {
            let mut name = self.text().to_string();
            self.bump();
            // A qualified type name — a nested class `Outer.Inner` (registered as `Outer.Inner`) or a
            // package-qualified type (`kotlin.reflect.KClass`). Consume the dotted path.
            while self.at(TokenKind::Dot)
                && self
                    .t
                    .get(self.i + 1)
                    .map_or(false, |t| t.kind == TokenKind::Ident)
            {
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
                    TypeRef {
                        name: "Any".to_string(),
                        nullable: true,
                        arg: None,
                        targs: Vec::new(),
                        span,
                        fun_params: Vec::new(),
                        fun_has_receiver: false,
                        fun_suspend: false,
                    }
                } else {
                    self.parse_type()
                };
                self.expect(TokenKind::Gt, "'>'");
                Some(Box::new(elem))
            } else {
                targs = self.parse_type_args(); // `Box<Int>` → carry `[Int]` (erased in descriptors)
                None
            };
            let nullable = self.eat_type_nullable(); // `T?`
            let base = TypeRef {
                name,
                nullable,
                arg,
                targs,
                span,
                fun_params: Vec::new(),
                fun_has_receiver: false,
                fun_suspend: false,
            };
            // Receiver (extension) function type `Recv.() -> R` ≡ `Function1<Recv, R>`, and
            // `Recv.(A) -> R` ≡ `Function2<Recv, A, R>`. The receiver folds in as the first function
            // parameter, exactly how Kotlin lowers an extension-function type to `FunctionN` — so the
            // rest of the pipeline sees a plain `(Recv, …) -> R`. (The dotted-path loop above stops at
            // `.` `(` since `(` is not an `Ident`, leaving us positioned here.)
            if self.at(TokenKind::Dot)
                && self
                    .t
                    .get(self.i + 1)
                    .map_or(false, |t| t.kind == TokenKind::LParen)
            {
                self.bump(); // '.'
                self.bump(); // '('
                let mut fun_params = vec![base];
                while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                    if self.at(TokenKind::Ident)
                        && self
                            .t
                            .get(self.i + 1)
                            .map_or(false, |t| t.kind == TokenKind::Colon)
                    {
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
                let fnull = self.eat_type_nullable();
                return TypeRef {
                    name: "<fun>".to_string(),
                    nullable: fnull,
                    arg: Some(Box::new(ret)),
                    targs: Vec::new(),
                    span,
                    fun_params,
                    fun_has_receiver: true,
                    fun_suspend,
                };
            }
            base
        } else {
            self.diags.error(span, "expected a type");
            TypeRef {
                name: "<error>".to_string(),
                nullable: false,
                arg: None,
                targs: Vec::new(),
                span,
                fun_params: Vec::new(),
                fun_has_receiver: false,
                fun_suspend: false,
            }
        }
    }

    /// Skip a leading `out`/`in` use-site variance modifier inside a type-argument list (`Array<in T>`,
    /// `List<out T>`). Variance is JVM-erased, so the projection is dropped and the bare type kept.
    /// `out` is a soft keyword (`Ident`); `in` is the real keyword `KwIn`.
    fn skip_variance(&mut self) {
        if self.at(TokenKind::KwIn) || (self.at(TokenKind::Ident) && self.text() == "out") {
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
                args.push(TypeRef {
                    name: "Any".to_string(),
                    nullable: true,
                    arg: None,
                    targs: Vec::new(),
                    span,
                    fun_params: Vec::new(),
                    fun_has_receiver: false,
                    fun_suspend: false,
                });
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
    #[allow(clippy::type_complexity)]
    fn parse_type_params(
        &mut self,
    ) -> (
        Vec<String>,
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
        Vec<(String, TypeRef)>,
    ) {
        let mut names = Vec::new();
        let mut non_null = std::collections::HashSet::new();
        let mut reified = std::collections::HashSet::new();
        let mut bounds: Vec<(String, TypeRef)> = Vec::new();
        if !self.eat(TokenKind::Lt) {
            return (names, non_null, reified, bounds);
        }
        loop {
            self.skip_newlines();
            // Skip variance/reified modifiers. `in` is a keyword; `out`/`reified` are idents.
            let mut is_reified = false;
            while (self.at(TokenKind::Ident) && matches!(self.text(), "reified" | "out"))
                || self.at(TokenKind::KwIn)
            {
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
                // A primitive upper bound (`T: Int`) is *specialized* by kotlinc (descriptor `(I)I`, not
                // `(Object)Object`); the resolver specializes a FUNCTION param to an integral bound. A
                // NON-specializable primitive bound (floating `Double`/`Float`, unsigned, value) is still
                // rejected — krusty would otherwise miscompile the boxed/primitive `==` or unsigned path.
                if !bound.nullable
                    && crate::types::Ty::from_name(&bound.name).is_some_and(|t| {
                        matches!(
                            t,
                            crate::types::Ty::Int
                                | crate::types::Ty::Byte
                                | crate::types::Ty::Short
                                | crate::types::Ty::Long
                                | crate::types::Ty::Float
                                | crate::types::Ty::Double
                                | crate::types::Ty::Boolean
                                | crate::types::Ty::Char
                                | crate::types::Ty::UInt
                                | crate::types::Ty::ULong
                        ) && !t.is_specializable_bound()
                    })
                {
                    self.diags.error(
                        bound.span,
                        "krusty: type parameter with this primitive upper bound is not supported"
                            .to_string(),
                    );
                }
                // Record an upper bound so a value class's underlying type parameter can take its bound's
                // type/nullability (`value class S<T: String>` → `String`; `<T: String?>`/`<T: Any?>` →
                // null-capable). A NON-NULL `Any` bound carries nothing useful (the erasure is already
                // `Object`); a NULLABLE `Any?` bound DOES (it makes the value class null-capable).
                if !tname.is_empty() && (bound.name != "Any" || bound.nullable) {
                    bounds.push((tname.clone(), bound));
                }
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "'>'");
        (names, non_null, reified, bounds)
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
        // A destructured lambda parameter `{ (a, b) -> … }` binds ONE (synthetic) parameter, then
        // `val (a, b) = <synthetic>` is prepended to the body — reusing the `Stmt::Destructure`
        // machinery. Collected here, spliced after the body statements are parsed.
        // (synthetic param name, destructured entries `(name, is_var)`, span) per `(a, b)` param.
        type LambdaDestructure = (String, Vec<(String, bool)>, Span);
        let mut destructures: Vec<LambdaDestructure> = Vec::new();
        let params = if has_params {
            let mut ps = Vec::new();
            loop {
                self.skip_newlines();
                if self.at(TokenKind::LParen) {
                    let sp = self.tok().span;
                    self.bump();
                    let mut entries = Vec::new();
                    loop {
                        let n = self.ident_or_error("variable name");
                        // A per-entry type annotation (`(a: Int, b) ->`) is tolerated, ignored.
                        if self.eat(TokenKind::Colon) {
                            let _ = self.parse_type();
                        }
                        entries.push((n, false)); // destructured lambda params are `val`
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        if self.at(TokenKind::RParen) {
                            break; // trailing comma
                        }
                    }
                    self.expect(TokenKind::RParen, "')'");
                    // A type annotation on the whole destructured parameter (`(a, b): T ->`) is ignored.
                    if self.eat(TokenKind::Colon) {
                        let _ = self.parse_type();
                    }
                    let synth = format!("$dstr{}", destructures.len());
                    ps.push(synth.clone());
                    param_types.push(None);
                    destructures.push((synth, entries, sp));
                } else if self.name_based_destructuring && self.at(TokenKind::LBracket) {
                    // The short-form bracket destructuring `{ [a, b] -> … }` (NameBasedDestructuring) —
                    // identical to the `(a, b)` form, just with `[ ]`.
                    let sp = self.tok().span;
                    self.bump();
                    let mut entries = Vec::new();
                    loop {
                        let n = self.ident_or_error("variable name");
                        if self.eat(TokenKind::Colon) {
                            let _ = self.parse_type();
                        }
                        entries.push((n, false));
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        if self.at(TokenKind::RBracket) {
                            break; // trailing comma
                        }
                    }
                    self.expect(TokenKind::RBracket, "']'");
                    if self.eat(TokenKind::Colon) {
                        let _ = self.parse_type();
                    }
                    let synth = format!("$dstr{}", destructures.len());
                    ps.push(synth.clone());
                    param_types.push(None);
                    destructures.push((synth, entries, sp));
                } else if self.at(TokenKind::Ident) {
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
        // Prepend `val (a, b) = <synthetic-param>` for each destructured parameter (reversed so the
        // first parameter's binding ends up first).
        for (synth, entries, sp) in destructures.into_iter().rev() {
            let init = self.file.add_expr(Expr::Name(synth), sp);
            let d = self.file.add_stmt(Stmt::Destructure { entries, init }, sp);
            stmts.insert(0, d);
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
        let body = self
            .file
            .add_expr(Expr::Block { stmts, trailing }, Span::new(start.lo, end.hi));
        let lam = self
            .file
            .add_expr(Expr::Lambda { params, body }, Span::new(start.lo, end.hi));
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
        self.file
            .add_expr(Expr::Block { stmts, trailing }, Span::new(start.lo, end.hi))
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
                && next2.map_or(false, |t| {
                    matches!(
                        t.kind,
                        TokenKind::KwWhile | TokenKind::KwFor | TokenKind::KwDo
                    )
                });
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
        if self.at(TokenKind::Ident)
            && self.text() == "lateinit"
            && self
                .t
                .get(self.i + 1)
                .map_or(false, |t| t.kind == TokenKind::KwVar)
        {
            self.diags.error(
                self.tok().span,
                "krusty: lateinit local variables are not supported",
            );
            self.bump(); // 'lateinit'
        }
        let start = self.tok().span;
        match self.kind() {
            TokenKind::KwVal | TokenKind::KwVar => {
                let is_var = self.at(TokenKind::KwVar);
                self.bump();
                // Destructuring declaration: `val (a, b, …) = init`, or the name-based `val [a, b] =
                // init` under `+NameBasedDestructuring` (both desugar to positional `componentN`).
                let close = if self.at(TokenKind::LParen) {
                    Some(TokenKind::RParen)
                } else if self.name_based_destructuring && self.at(TokenKind::LBracket) {
                    Some(TokenKind::RBracket)
                } else {
                    None
                };
                if let Some(close) = close {
                    self.bump();
                    let mut entries = Vec::new();
                    loop {
                        let n = self.ident_or_error("variable name");
                        // A per-entry type annotation (`val (a: Int, b) = …`) is tolerated, ignored.
                        if self.eat(TokenKind::Colon) {
                            let _ = self.parse_type();
                        }
                        entries.push((n, is_var));
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        if self.at(close) {
                            break;
                        } // trailing comma
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
                    return self.finish_stmt(Stmt::Destructure { entries, init }, start);
                }
                let name = self.ident_or_error("variable name");
                let ty = if self.eat(TokenKind::Colon) {
                    Some(self.parse_type())
                } else {
                    None
                };
                // `val/var x (: T)? by <delegate>` — a local delegated property.
                if self.at(TokenKind::Ident) && self.text() == "by" {
                    self.bump(); // 'by'
                    self.skip_newlines();
                    let delegate = self.parse_expr();
                    return self.finish_stmt(
                        Stmt::LocalDelegate {
                            is_var,
                            name,
                            ty,
                            delegate,
                        },
                        start,
                    );
                }
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
                self.finish_stmt(
                    Stmt::Local {
                        is_var,
                        name,
                        ty,
                        init,
                    },
                    start,
                )
            }
            TokenKind::KwReturn => {
                self.bump();
                // `return@label` — a local return from the lambda carrying `label` (`return@forEach`).
                let label = if self.at(TokenKind::At) {
                    self.bump(); // '@'
                    if self.at(TokenKind::Ident) {
                        let l = self.text().to_string();
                        self.bump();
                        Some(l)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let e = if self.at(TokenKind::Newline)
                    || self.at(TokenKind::RBrace)
                    || self.at(TokenKind::Eof)
                {
                    None
                } else {
                    Some(self.parse_expr())
                };
                self.finish_stmt(Stmt::Return(e, label), start)
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
                self.finish_stmt(
                    Stmt::While {
                        cond,
                        body,
                        label: loop_label,
                    },
                    start,
                )
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
                self.finish_stmt(
                    Stmt::DoWhile {
                        body,
                        cond,
                        label: loop_label,
                    },
                    start,
                )
            }
            TokenKind::KwFor => self.parse_for(start, loop_label),
            // Local function declaration: `fun name(params): Ret { body }` inside a function body.
            TokenKind::KwFun => {
                // Local functions don't carry a `suspend` modifier through this path; a local
                // `suspend fun` is handled (skipped) downstream via the suspend guard in lowering.
                let f = self.parse_fun(false, false, false, false);
                self.finish_stmt(Stmt::LocalFun(f), start)
            }
            // Local class declaration inside a function body (`class`/`data class`/`enum class`/
            // `sealed class`/`annotation class`/`interface Name`, optionally `open`/`abstract`/… prefixed).
            // Consume leading modifiers/annotations (as the top-level path does), then apply `open`/
            // `abstract` to the parsed decl. (`object` is omitted — a bare `object` may start an
            // anonymous-object EXPRESSION, which stays on the expression path.)
            _ if self.looks_like_local_type_decl() => {
                let mods = self.skip_decl_prefix();
                let is_sealed = mods.iter().any(|m| m == "sealed");
                let is_open = is_sealed || mods.iter().any(|m| m == "open");
                let is_abstract = is_sealed || mods.iter().any(|m| m == "abstract");
                let mut d = self.parse_nested_type_decl();
                // Preserves the prior behavior: this path applied open/abstract but left `is_sealed`
                // at its default `false` (so a local `sealed` class never reported `is_sealed`).
                d.modality = modality_of(is_open, is_abstract, false);
                self.finish_stmt(Stmt::LocalClass(d), start)
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
                            return self.finish_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                },
                                start,
                            );
                        }
                        Expr::Index { array, index } => {
                            self.bump(); // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            return self.finish_stmt(
                                Stmt::AssignIndex {
                                    array,
                                    index,
                                    value,
                                },
                                start,
                            );
                        }
                        _ => self
                            .diags
                            .error(self.tok().span, "invalid assignment target"),
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
                            let lhs = self.file.add_expr(
                                Expr::Member {
                                    receiver,
                                    name: name.clone(),
                                },
                                op_span,
                            );
                            let value = self.file.add_expr(Expr::Binary { op, lhs, rhs }, op_span);
                            return self.finish_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                },
                                start,
                            );
                        }
                        Expr::Index { array, index } => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_expr();
                            let lhs = self.file.add_expr(Expr::Index { array, index }, op_span);
                            let value = self.file.add_expr(Expr::Binary { op, lhs, rhs }, op_span);
                            return self.finish_stmt(
                                Stmt::AssignIndex {
                                    array,
                                    index,
                                    value,
                                },
                                start,
                            );
                        }
                        _ => self
                            .diags
                            .error(self.tok().span, "invalid assignment target"),
                    }
                }
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    fn parse_for(&mut self, start: Span, label: Option<String>) -> StmtId {
        self.bump(); // 'for'
        self.expect(TokenKind::LParen, "'('");
        // A destructuring loop variable — `for ((a, b) in pairs)`, or the name-based `for ([a, b] in
        // pairs)` under `+NameBasedDestructuring` — desugars to a synthetic temp plus `val (a, b) =
        // temp` prepended to the body (reusing the `Stmt::Destructure` machinery; both forms lower to
        // the same positional `componentN` calls, so the bytecode matches kotlinc's either way).
        let close = if self.at(TokenKind::LParen) {
            Some(TokenKind::RParen)
        } else if self.name_based_destructuring && self.at(TokenKind::LBracket) {
            Some(TokenKind::RBracket)
        } else {
            None
        };
        let destructure: Option<Vec<(String, bool)>> = if let Some(close) = close {
            self.bump();
            let mut entries = Vec::new();
            loop {
                let n = self.ident_or_error("variable name");
                if self.eat(TokenKind::Colon) {
                    let _ = self.parse_type();
                }
                entries.push((n, false));
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
            // No range operator: a plain iterable. It may still carry trailing infix calls that the
            // bp-9 start didn't consume (`for (x in progression step 2)`, `… step 2 step 0`) — continue
            // them so the whole expression (e.g. `progression.step(2)`) becomes the ForEach iterable.
            let rstart = self.parse_for_trailing_infix(rstart);
            self.expect(TokenKind::RParen, "')'");
            self.skip_newlines();
            let body = self.parse_branch();
            let body = self.desugar_destructure_body(&name, destructure, body);
            // `for (i in X.indices)` → counted loop `0 until X.size`.
            if let Expr::Member {
                receiver,
                name: mname,
            } = self.file.expr(rstart).clone()
            {
                if mname == "indices" {
                    let sp = self.file.expr_spans[rstart.0 as usize];
                    let zero = self.file.add_expr(Expr::IntLit(0), sp);
                    let size = self.file.add_expr(
                        Expr::Member {
                            receiver,
                            name: "size".to_string(),
                        },
                        sp,
                    );
                    let range = ForRange {
                        start: zero,
                        end: size,
                        kind: RangeKind::Until,
                    };
                    return self.finish_stmt(
                        Stmt::For {
                            name,
                            range,
                            body,
                            label,
                        },
                        start,
                    );
                }
            }
            // `for (i in (a..b).reversed())` / `(a downTo b).reversed()` → the reversed counted loop
            // (`b downTo a` / `b..a`). Only a *literal* `..`/`downTo` range is rewritten here (step-1);
            // a stepped or `until` reversal, or a stored progression, keeps the iterable path (skips).
            if let Expr::Call { callee, args } = self.file.expr(rstart).clone() {
                if args.is_empty() {
                    if let Expr::Member {
                        receiver,
                        name: mname,
                    } = self.file.expr(callee).clone()
                    {
                        if mname == "reversed" {
                            // Reversing swaps which bound is evaluated first, so only rewrite when both
                            // bounds are side-effect-free (a literal or a name) — kotlinc evaluates a
                            // reversed range's bounds in SOURCE order, so a call-bound
                            // `(logged()..logged()).reversed()` keeps the iterable path.
                            let simple = |p: &Self, id: ExprId| {
                                matches!(
                                    p.file.expr(id),
                                    Expr::IntLit(_)
                                        | Expr::LongLit(_)
                                        | Expr::UIntLit(_)
                                        | Expr::ULongLit(_)
                                        | Expr::CharLit(_)
                                        | Expr::Name(_)
                                )
                            };
                            // The reversed range as `(start_base, end, kind, minus_one)`: a
                            // `..`/`downTo`/`until` literal flips to the descending/ascending counted
                            // loop. `..` is `RangeTo`; the value-form `downTo`/`until` parse as infix
                            // calls `a.downTo(b)`. `until`-reversed iterates `(hi-1) downTo lo`, so
                            // `minus_one` subtracts 1 from `start_base` AFTER the simplicity check (which
                            // is on the ORIGINAL bound, not the derived `hi-1`).
                            let reversed: Option<(ExprId, ExprId, RangeKind, bool)> = match self
                                .file
                                .expr(receiver)
                                .clone()
                            {
                                Expr::RangeTo { lo, hi, kind } => match kind {
                                    RangeKind::Through => Some((hi, lo, RangeKind::DownTo, false)),
                                    RangeKind::DownTo => Some((hi, lo, RangeKind::Through, false)),
                                    RangeKind::Until => Some((hi, lo, RangeKind::DownTo, true)),
                                },
                                // The value-form `(a downTo b)` / `(a until b)` parse as infix calls.
                                Expr::Call {
                                    callee: ic,
                                    args: ia,
                                } if ia.len() == 1 => match self.file.expr(ic).clone() {
                                    Expr::Member {
                                        receiver: a,
                                        name: op,
                                    } if op == "downTo" => {
                                        Some((ia[0], a, RangeKind::Through, false))
                                    }
                                    Expr::Member {
                                        receiver: a,
                                        name: op,
                                    } if op == "until" => Some((ia[0], a, RangeKind::DownTo, true)),
                                    _ => None,
                                },
                                _ => None,
                            };
                            if let Some((start_base, en, kind, minus_one)) = reversed {
                                if simple(self, start_base) && simple(self, en) {
                                    // `until`-reversed: descending from `hi-1`.
                                    let s = if minus_one {
                                        let sp = self.file.expr_spans[start_base.0 as usize];
                                        let one = self.file.add_expr(Expr::IntLit(1), sp);
                                        self.file.add_expr(
                                            Expr::Binary {
                                                op: BinOp::Sub,
                                                lhs: start_base,
                                                rhs: one,
                                            },
                                            sp,
                                        )
                                    } else {
                                        start_base
                                    };
                                    let range = ForRange {
                                        start: s,
                                        end: en,
                                        kind,
                                    };
                                    return self.finish_stmt(
                                        Stmt::For {
                                            name,
                                            range,
                                            body,
                                            label,
                                        },
                                        start,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            // Otherwise iterate over `rstart` as a collection: `for (x in array)`.
            return self.finish_stmt(
                Stmt::ForEach {
                    name,
                    iterable: rstart,
                    body,
                    label,
                },
                start,
            );
        };
        let rend = self.parse_bp(9);
        // A `..`/`until`/`downTo` range may be followed by ordinary infix calls (`step`, or any user
        // infix), possibly chained (`a..b step 2 step 3`). These are NOT special syntax — recognizing
        // them here by name would be the hardcode kotlinc avoids. Build the base range value and apply
        // any trailing infix generically; the result's TYPE (e.g. `IntProgression` from `step`) drives
        // the loop lowering. A bare range (no trailing infix) keeps the optimized counted `Stmt::For`.
        if self.at(TokenKind::Ident) && {
            let next = self.t.get(self.i + 1).is_some_and(|t| starts_expr(t.kind));
            !matches!(self.text(), "is" | "as" | "in") && next
        } {
            let lspan = self.file.expr_spans[rstart.0 as usize];
            let rspan = self.file.expr_spans[rend.0 as usize];
            let base_span = Span::new(lspan.lo, rspan.hi);
            // The base range value: `..`/`until` are `RangeTo`; `downTo` is its infix call form.
            let base = match kind {
                RangeKind::DownTo => {
                    let callee = self.file.add_expr(
                        Expr::Member {
                            receiver: rstart,
                            name: "downTo".to_string(),
                        },
                        base_span,
                    );
                    self.file.add_expr(
                        Expr::Call {
                            callee,
                            args: vec![rend],
                        },
                        base_span,
                    )
                }
                k => self.file.add_expr(
                    Expr::RangeTo {
                        lo: rstart,
                        hi: rend,
                        kind: k,
                    },
                    base_span,
                ),
            };
            let iterable = self.parse_for_trailing_infix(base);
            self.expect(TokenKind::RParen, "')'");
            self.skip_newlines();
            let body = self.parse_branch();
            let body = self.desugar_destructure_body(&name, destructure, body);
            return self.finish_stmt(
                Stmt::ForEach {
                    name,
                    iterable,
                    body,
                    label,
                },
                start,
            );
        }
        self.expect(TokenKind::RParen, "')'");
        self.skip_newlines();
        let body = self.parse_branch();
        self.finish_stmt(
            Stmt::For {
                name,
                range: ForRange {
                    start: rstart,
                    end: rend,
                    kind,
                },
                body,
                label,
            },
            start,
        )
    }

    /// Apply trailing infix function calls to a `for`-loop iterable base: each `name rhs` becomes
    /// `recv.name(rhs)`, chaining left-to-right (`p step 2 step 0` → `(p.step(2)).step(0)`). The
    /// operand is parsed at additive precedence so a following infix starts a new call rather than
    /// being swallowed. These are ordinary functions — the loop lowering keys off the resulting
    /// value's type, never the function name.
    fn parse_for_trailing_infix(&mut self, mut recv: ExprId) -> ExprId {
        while self.at(TokenKind::Ident) {
            let name = self.text();
            let next_starts_expr = self.t.get(self.i + 1).is_some_and(|t| starts_expr(t.kind));
            if matches!(name, "is" | "as" | "in") || !next_starts_expr {
                break;
            }
            let name = name.to_string();
            let lspan = self.file.expr_spans[recv.0 as usize];
            self.bump(); // infix function name
            self.skip_newlines();
            let rhs = self.parse_bp(9);
            let rspan = self.file.expr_spans[rhs.0 as usize];
            let callee = self.file.add_expr(
                Expr::Member {
                    receiver: recv,
                    name,
                },
                Span::new(lspan.lo, rspan.hi),
            );
            recv = self.file.add_expr(
                Expr::Call {
                    callee,
                    args: vec![rhs],
                },
                Span::new(lspan.lo, rspan.hi),
            );
            self.file.infix_calls.insert(recv.0);
        }
        recv
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
    fn desugar_destructure_body(
        &mut self,
        temp: &str,
        destructure: Option<Vec<(String, bool)>>,
        body: ExprId,
    ) -> ExprId {
        let Some(entries) = destructure else {
            return body;
        };
        let sp = self.file.expr_spans[body.0 as usize];
        let temp_expr = self.file.add_expr(Expr::Name(temp.to_string()), sp);
        let dstmt = self.file.add_stmt(
            Stmt::Destructure {
                entries,
                init: temp_expr,
            },
            sp,
        );
        match self.file.expr(body).clone() {
            Expr::Block { stmts, trailing } => {
                let mut s2 = vec![dstmt];
                s2.extend(stmts);
                self.file.add_expr(
                    Expr::Block {
                        stmts: s2,
                        trailing,
                    },
                    sp,
                )
            }
            _ => self.file.add_expr(
                Expr::Block {
                    stmts: vec![dstmt],
                    trailing: Some(body),
                },
                sp,
            ),
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
            self.diags
                .error(self.tok().span, format!("expected {what}"));
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
                Some(TokenKind::Lt) => {
                    depth += 1;
                    j += 1;
                }
                Some(TokenKind::Gt) => {
                    depth -= 1;
                    j += 1;
                    if depth == 0 {
                        break;
                    }
                }
                // `>=` closes the last `<` if depth == 1 (e.g. `Foo<Bar>=` — not valid type args).
                // Treat as "not type args" to stay safe.
                Some(TokenKind::GtEq) => return false,
                // Tokens valid inside type argument lists — including a function-type argument
                // (`Foo<(A) -> B>`): its parens and arrow.
                Some(TokenKind::Ident)
                | Some(TokenKind::Dot)
                | Some(TokenKind::Comma)
                | Some(TokenKind::Star)
                | Some(TokenKind::Question)
                | Some(TokenKind::Colon)
                | Some(TokenKind::LParen)
                | Some(TokenKind::RParen)
                | Some(TokenKind::Arrow) => {
                    j += 1;
                }
                _ => return false,
            }
        }
        // After `>`, must be followed by `(`, `{`, or `.` to be a generic call.
        matches!(
            self.t.get(j).map(|t| t.kind),
            Some(TokenKind::LParen) | Some(TokenKind::LBrace) | Some(TokenKind::Dot)
        )
    }

    // ---- expressions (Pratt) ----
    fn parse_expr(&mut self) -> ExprId {
        self.parse_elvis()
    }

    /// Elvis `?:` is the lowest-precedence binary operator (below `||`).
    fn parse_elvis(&mut self) -> ExprId {
        let mut lhs = self.parse_bp(0);
        while self.at(TokenKind::Question)
            && self
                .t
                .get(self.i + 1)
                .map_or(false, |t| t.kind == TokenKind::Colon)
        {
            self.bump(); // '?'
            self.bump(); // ':'
            self.skip_newlines();
            let rhs = self.parse_bp(0);
            let lspan = self.file.expr_spans[lhs.0 as usize];
            let rspan = self.file.expr_spans[rhs.0 as usize];
            lhs = self
                .file
                .add_expr(Expr::Elvis { lhs, rhs }, Span::new(lspan.lo, rspan.hi));
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
                    && self.t.get(self.i + 1).map_or(false, |t| {
                        t.kind == TokenKind::Ident && t.text(self.src) == "is"
                    })
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
                    lhs = self.file.add_expr(
                        Expr::Is {
                            operand: lhs,
                            ty,
                            negated,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
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
                    && self
                        .t
                        .get(self.i + 1)
                        .map_or(false, |t| t.kind == TokenKind::KwIn)
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
                            lhs = self.file.add_expr(
                                Expr::InRange {
                                    value: lhs,
                                    start: rstart,
                                    end: rend,
                                    kind,
                                    negated,
                                },
                                Span::new(lspan.lo, end.hi),
                            );
                        }
                        None => {
                            // `value in container` → `container.contains(value)`.
                            let cspan = self.file.expr_spans[rstart.0 as usize];
                            let callee = self.file.add_expr(
                                Expr::Member {
                                    receiver: rstart,
                                    name: "contains".to_string(),
                                },
                                Span::new(lspan.lo, cspan.hi),
                            );
                            let call = self.file.add_expr(
                                Expr::Call {
                                    callee,
                                    args: vec![lhs],
                                },
                                Span::new(lspan.lo, cspan.hi),
                            );
                            lhs = if negated {
                                self.file.add_expr(
                                    Expr::Unary {
                                        op: UnOp::Not,
                                        operand: call,
                                    },
                                    Span::new(lspan.lo, cspan.hi),
                                )
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
                    lhs = self.file.add_expr(
                        Expr::RangeTo { lo: lhs, hi, kind },
                        Span::new(lspan.lo, rspan.hi),
                    );
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
                let next_starts_expr = self
                    .t
                    .get(self.i + 1)
                    .map_or(false, |t| starts_expr(t.kind));
                if !is_soft_kw && next_starts_expr {
                    let name = name.to_string();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // infix function name
                    self.skip_newlines();
                    let rhs = self.parse_bp(9); // operand binds at additive precedence or tighter
                    let rspan = self.file.expr_spans[rhs.0 as usize];
                    let callee = self.file.add_expr(
                        Expr::Member {
                            receiver: lhs,
                            name,
                        },
                        Span::new(lspan.lo, rspan.hi),
                    );
                    lhs = self.file.add_expr(
                        Expr::Call {
                            callee,
                            args: vec![rhs],
                        },
                        Span::new(lspan.lo, rspan.hi),
                    );
                    self.file.infix_calls.insert(lhs.0);
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
            lhs = self
                .file
                .add_expr(Expr::Binary { op, lhs, rhs }, Span::new(lspan.lo, rspan.hi));
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
            return self
                .file
                .add_expr(Expr::Throw { operand }, Span::new(start.lo, end.hi));
        }
        // `return`/`return@label value` in expression position (`x ?: return null`).
        if self.at(TokenKind::KwReturn) {
            self.bump(); // 'return'
            let label = if self.at(TokenKind::At) {
                self.bump();
                if self.at(TokenKind::Ident) {
                    let l = self.text().to_string();
                    self.bump();
                    Some(l)
                } else {
                    None
                }
            } else {
                None
            };
            // A value follows unless the next token closes the expression context.
            let value = if matches!(
                self.kind(),
                TokenKind::Newline
                    | TokenKind::RBrace
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::Comma
                    | TokenKind::Eof
            ) {
                None
            } else {
                Some(self.parse_bp(0))
            };
            let end = value
                .map(|v| self.file.expr_spans[v.0 as usize])
                .unwrap_or(start);
            return self
                .file
                .add_expr(Expr::Return { value, label }, Span::new(start.lo, end.hi));
        }
        let unop = match self.kind() {
            TokenKind::Minus => Some(UnOp::Neg),
            TokenKind::Not => Some(UnOp::Not),
            TokenKind::Plus => Some(UnOp::Plus),
            _ => None,
        };
        if let Some(op) = unop {
            self.bump();
            // Kotlin: `-2147483648` is `Int.MIN_VALUE` (an `Int`), even though the bare literal
            // `2147483648` overflows `Int` and is otherwise a `Long`. Fold this one case so the
            // negation keeps `Int` type (a `when (x: Int)` branch / `val i: Int = -2147483648`).
            if matches!(op, UnOp::Neg)
                && self.at(TokenKind::IntLit)
                && parse_int_literal(self.text()) == 2147483648
            {
                let lit_span = self.tok().span;
                self.bump();
                return self.file.add_expr(
                    Expr::IntLit(i32::MIN as i64),
                    Span::new(start.lo, lit_span.hi),
                );
            }
            let operand = self.parse_bp(BP_PREFIX);
            let end = self.file.expr_spans[operand.0 as usize];
            return self
                .file
                .add_expr(Expr::Unary { op, operand }, Span::new(start.lo, end.hi));
        }
        // Prefix `++target` / `--target` as a value (the new value). Statement position is intercepted
        // in `parse_stmt` before reaching here, so this fires only when used as a value.
        if self.at(TokenKind::PlusPlus) || self.at(TokenKind::MinusMinus) {
            let dec = self.at(TokenKind::MinusMinus);
            self.bump();
            let target = self.parse_bp(BP_PREFIX);
            let end = self.file.expr_spans[target.0 as usize];
            return self.file.add_expr(
                Expr::IncDec {
                    target,
                    dec,
                    prefix: true,
                },
                Span::new(start.lo, end.hi),
            );
        }
        let primary = self.parse_primary();
        self.parse_postfix(primary)
    }

    fn parse_postfix(&mut self, mut lhs: ExprId) -> ExprId {
        // Explicit type arguments parsed just before a call paren (`foo<Int>(…)`), attached to the
        // call once it is built so a constructor instantiation (`ArrayList<Int>()`) keeps its args.
        let mut pending_targs: Vec<TypeRef> = Vec::new();
        loop {
            // A postfix chain may continue on a following line: Kotlin treats a newline before `.` or
            // `?.` as part of the selector chain, not a statement terminator (`x\n  .foo()\n  .bar()`).
            // Peek past the newline(s); if a member access follows, consume them and continue —
            // otherwise stop (the expression ended). `::`/`{` deliberately do NOT continue across a
            // newline (callable-ref / trailing-lambda are same-line only).
            if self.at(TokenKind::Newline) {
                let mut j = self.i;
                while self.t.get(j).is_some_and(|t| t.kind == TokenKind::Newline) {
                    j += 1;
                }
                let continues = match self.t.get(j).map(|t| t.kind) {
                    Some(TokenKind::Dot) => true,
                    Some(TokenKind::Question) => {
                        self.t.get(j + 1).is_some_and(|t| t.kind == TokenKind::Dot)
                    }
                    _ => false,
                };
                if !continues {
                    break;
                }
                self.skip_newlines();
            }
            // `as T` / `as? T` cast — binds tighter than the binary operators (postfix level).
            if self.at(TokenKind::Ident) && self.text() == "as" {
                let lspan = self.file.expr_spans[lhs.0 as usize];
                self.bump(); // 'as'
                let nullable = self.eat_type_nullable();
                let ty = self.parse_type();
                let end = self.t[self.i.saturating_sub(1)].span;
                lhs = self.file.add_expr(
                    Expr::As {
                        operand: lhs,
                        ty,
                        nullable,
                    },
                    Span::new(lspan.lo, end.hi),
                );
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
                    lhs = self.file.add_expr(
                        Expr::IncDec {
                            target: lhs,
                            dec,
                            prefix: false,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                }
                // `!!` not-null assertion in postfix position = two consecutive `Not` tokens.
                TokenKind::Not
                    if self
                        .t
                        .get(self.i + 1)
                        .map_or(false, |t| t.kind == TokenKind::Not) =>
                {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump();
                    let end = self.tok().span;
                    self.bump();
                    lhs = self
                        .file
                        .add_expr(Expr::NotNull { operand: lhs }, Span::new(lspan.lo, end.hi));
                }
                // `?.` safe call: `recv?.name` or `recv?.name(args)`.
                TokenKind::Question
                    if self
                        .t
                        .get(self.i + 1)
                        .map_or(false, |t| t.kind == TokenKind::Dot) =>
                {
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
                    lhs = self.file.add_expr(
                        Expr::SafeCall {
                            receiver: lhs,
                            name,
                            args,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                }
                // `Recv?::name` / `Recv?::class` — a callable reference / class literal on a NULLABLE
                // receiver type. The `?` only marks the receiver type nullable; the reference is the same
                // callable, so parse it as the bound reference (krusty's `CallableRef` ignores the `?`).
                TokenKind::Question
                    if self
                        .t
                        .get(self.i + 1)
                        .map_or(false, |t| t.kind == TokenKind::ColonColon) =>
                {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // '?'
                    self.bump(); // '::'
                    let name = if self.at(TokenKind::Ident) {
                        let n = self.text().to_string();
                        self.bump();
                        n
                    } else if self.at(TokenKind::KwClass) {
                        self.bump();
                        "class".to_string()
                    } else {
                        "<error>".to_string()
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(
                        Expr::CallableRef {
                            receiver: Some(lhs),
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                }
                TokenKind::Dot => {
                    self.bump();
                    let name = self.ident_or_error("member name");
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(
                        Expr::Member {
                            receiver: lhs,
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                }
                // `expr::name` or `Expr::class` — bound callable reference / class literal.
                TokenKind::ColonColon => {
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    self.bump(); // '::'
                    let name = if self.at(TokenKind::Ident) {
                        let n = self.text().to_string();
                        self.bump();
                        n
                    } else if self.at(TokenKind::KwClass) {
                        self.bump();
                        "class".to_string()
                    } else {
                        "<error>".to_string()
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    lhs = self.file.add_expr(
                        Expr::CallableRef {
                            receiver: Some(lhs),
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
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
                            && self
                                .t
                                .get(self.i + 1)
                                .map_or(false, |t| t.kind == TokenKind::Eq)
                        {
                            let n = self.text().to_string();
                            self.bump(); // name
                            self.bump(); // '='
                            self.skip_newlines();
                            names.push(Some(n));
                        } else {
                            names.push(None);
                        }
                        // Spread operator `*expr` — the argument is an array spread into a `vararg`.
                        let spread = self.eat(TokenKind::Star);
                        let arg = self.parse_expr();
                        if spread {
                            self.file.spread_arg_ids.insert(arg.0);
                        }
                        args.push(arg);
                        self.skip_newlines();
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.expect(TokenKind::RParen, "')'");
                    let call = self.file.add_expr(
                        Expr::Call { callee: lhs, args },
                        Span::new(lspan.lo, end.hi),
                    );
                    if names.iter().any(|n| n.is_some()) {
                        self.file.call_arg_names.insert(call.0, names);
                    }
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(call.0, std::mem::take(&mut pending_targs));
                    }
                    lhs = call;
                }
                // Trailing lambda: `expr { … }` / `recv.m(args) { … }` → append the lambda as the
                // last call argument (same line only, to avoid swallowing an unrelated block).
                TokenKind::LBrace if self.no_trailing_lambda => break,
                TokenKind::LBrace => {
                    let lambda = self.parse_lambda();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    let old = lhs;
                    lhs = match self.file.expr(lhs).clone() {
                        Expr::Call { callee, mut args } => {
                            args.push(lambda);
                            self.file
                                .add_expr(Expr::Call { callee, args }, Span::new(lspan.lo, end.hi))
                        }
                        // `recv?.scopeFn { … }` — the trailing lambda is the safe call's argument, not an
                        // invocation of its result. Attach it (appending after any `(…)` args).
                        Expr::SafeCall {
                            receiver,
                            name,
                            args,
                        } => {
                            let mut a = args.unwrap_or_default();
                            a.push(lambda);
                            self.file.add_expr(
                                Expr::SafeCall {
                                    receiver,
                                    name,
                                    args: Some(a),
                                },
                                Span::new(lspan.lo, end.hi),
                            )
                        }
                        _ => self.file.add_expr(
                            Expr::Call {
                                callee: lhs,
                                args: vec![lambda],
                            },
                            Span::new(lspan.lo, end.hi),
                        ),
                    };
                    // Carry any named-argument metadata to the rebuilt call (the trailing lambda is
                    // an extra positional argument).
                    if let Some(mut names) = self.file.call_arg_names.remove(&old.0) {
                        names.push(None);
                        self.file.call_arg_names.insert(lhs.0, names);
                    }
                    // Mark this call as having a SYNTACTIC trailing lambda so default-omission lowering
                    // binds it to the callee's LAST parameter (preceding gaps take defaults).
                    self.file.call_has_trailing_lambda.remove(&old.0);
                    self.file.call_has_trailing_lambda.insert(lhs.0);
                    // Carry explicit type arguments onto the rebuilt call — both the `f<T>(args){…}` form
                    // (already consumed into `old`'s entry by the paren branch) and the `f<T>{…}` form
                    // (no parens — `pending_targs` is still unconsumed). Without this, a trailing lambda
                    // drops `<T>` (e.g. `Array<T>(n){…}`, `f<T>{…}`).
                    if let Some(targs) = self.file.call_type_args.remove(&old.0) {
                        self.file.call_type_args.insert(lhs.0, targs);
                    }
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(lhs.0, std::mem::take(&mut pending_targs));
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
                    lhs = self.file.add_expr(
                        Expr::Index { array: lhs, index },
                        Span::new(lspan.lo, end.hi),
                    );
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
            && self
                .t
                .get(self.i + 1)
                .map_or(false, |t| t.kind == TokenKind::LBrace)
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
                let fname = if args.is_empty() {
                    "emptyArray"
                } else {
                    "arrayOf"
                };
                let callee = self.file.add_expr(Expr::Name(fname.to_string()), span);
                self.file
                    .add_expr(Expr::Call { callee, args }, Span::new(span.lo, end.hi))
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
                let v = parse_unsigned_literal_bits(self.text()); // suffix stripped inside
                self.bump();
                // A `U`-suffixed literal (no `L`) is `UInt` if it fits, else `ULong` (Kotlin's rule):
                // e.g. `0xffff_ffff_ffffU` exceeds `UInt.MAX` so it's a `ULong`.
                if v > u32::MAX as u64 {
                    self.file.add_expr(Expr::ULongLit(v as i64), span)
                } else {
                    self.file.add_expr(Expr::UIntLit(v as i64), span)
                }
            }
            TokenKind::ULongLit => {
                let v = parse_unsigned_literal_bits(self.text()) as i64;
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
                    && self.t.get(self.i + 1).map_or(false, |t| {
                        matches!(t.kind, TokenKind::Colon | TokenKind::LBrace)
                    }) =>
            {
                self.parse_anon_object(span)
            }
            TokenKind::Ident => {
                let n = self.text().to_string();
                self.bump();
                // A LABELED `this`/`super` (`this@Outer`, `super@Base`): the `@label` qualifies which
                // enclosing receiver / supertype it denotes. Capture it on the name (`this@Outer`) so the
                // checker/lowerer can resolve the label; a bare `this`/`super` stays unchanged.
                if (n == "this" || n == "super")
                    && self.at(TokenKind::At)
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::Ident)
                {
                    self.bump(); // '@'
                    let label = self.text().to_string();
                    self.bump(); // label
                    return self.file.add_expr(Expr::Name(format!("{n}@{label}")), span);
                }
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
                    let n = self.text().to_string();
                    self.bump();
                    n
                } else if self.at(TokenKind::KwClass) {
                    self.bump();
                    "class".to_string()
                } else {
                    "<error>".to_string()
                };
                self.file.add_expr(
                    Expr::CallableRef {
                        receiver: None,
                        name,
                    },
                    span,
                )
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
        self.file.add_expr(
            Expr::If {
                cond,
                then_branch,
                else_branch,
            },
            Span::new(start.lo, end.hi),
        )
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
        self.file
            .add_expr(Expr::Template(parts), Span::new(start.lo, end.hi))
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
                catches.push(CatchClause {
                    name,
                    ty,
                    body: cbody,
                });
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
            self.diags
                .error(start, "try without a catch or finally is not supported");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        self.file.add_expr(
            Expr::Try {
                body,
                catches,
                finally,
            },
            Span::new(start.lo, end.hi),
        )
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
                let lhs = self.file.add_expr(
                    Expr::Member {
                        receiver,
                        name: name.clone(),
                    },
                    op_span,
                );
                let value = self
                    .file
                    .add_expr(Expr::Binary { op, lhs, rhs: one }, op_span);
                self.finish_stmt(
                    Stmt::AssignMember {
                        receiver,
                        name,
                        value,
                    },
                    start,
                )
            }
            Expr::Index { array, index }
                if self.is_pure_path(array) && self.is_pure_path(index) =>
            {
                let one = self.file.add_expr(Expr::IntLit(1), op_span);
                let lhs = self.file.add_expr(Expr::Index { array, index }, op_span);
                let value = self
                    .file
                    .add_expr(Expr::Binary { op, lhs, rhs: one }, op_span);
                self.finish_stmt(
                    Stmt::AssignIndex {
                        array,
                        index,
                        value,
                    },
                    start,
                )
            }
            _ => {
                self.diags.error(
                    op_span,
                    "krusty: '++'/'--' is only supported on a simple variable or pure access path",
                );
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    /// Whether `e` is a side-effect-free access path — a name, a literal, or a member/index chain
    /// bottoming out at one. Such an expression can be re-evaluated safely (used to gate the
    /// `++`/`--` desugar, which reads its target twice).
    fn is_pure_path(&self, e: ExprId) -> bool {
        match self.file.expr(e) {
            Expr::Name(_)
            | Expr::IntLit(_)
            | Expr::LongLit(_)
            | Expr::CharLit(_)
            | Expr::BoolLit(_)
            | Expr::NullLit => true,
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
                let ty = if self.eat(TokenKind::Colon) {
                    Some(self.parse_type())
                } else {
                    None
                };
                self.expect(TokenKind::Eq, "'='");
                let init = self.parse_expr();
                self.expect(TokenKind::RParen, "')'");
                let sp = Span::new(vstart.lo, self.file.expr_spans[init.0 as usize].hi);
                let stmt = self.file.add_stmt(
                    Stmt::Local {
                        is_var,
                        name: name.clone(),
                        ty,
                        init,
                    },
                    sp,
                );
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
            Some((stmt, _)) => self.file.add_expr(
                Expr::Block {
                    stmts: vec![stmt],
                    trailing: Some(when_expr),
                },
                span,
            ),
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
            && self.t.get(self.i + 1).map_or(false, |t| {
                t.kind == TokenKind::Ident && t.text(self.src) == "is"
            })
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
            return self.file.add_expr(
                Expr::Is {
                    operand: subj,
                    ty,
                    negated,
                },
                Span::new(start.lo, end.hi),
            );
        }
        // `when (x) { in range -> … }` / `!in` — a membership condition on the subject (`x in range`),
        // mirroring the infix `in`/`!in` operator: a range RHS → `InRange`, any other RHS → `contains`.
        let in_negated = if self.at(TokenKind::KwIn) {
            Some(false)
        } else if self.at(TokenKind::Not)
            && self
                .t
                .get(self.i + 1)
                .map_or(false, |t| t.kind == TokenKind::KwIn)
        {
            Some(true)
        } else {
            None
        };
        if let (Some(negated), Some(subj)) = (in_negated, subject) {
            let start = self.tok().span;
            if negated {
                self.bump(); // '!'
            }
            self.bump(); // 'in'
            self.skip_newlines();
            let rstart = self.parse_bp(9);
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
            return match kind {
                Some(kind) => {
                    let rend = self.parse_bp(9);
                    let end = self.file.expr_spans[rend.0 as usize];
                    self.file.add_expr(
                        Expr::InRange {
                            value: subj,
                            start: rstart,
                            end: rend,
                            kind,
                            negated,
                        },
                        Span::new(start.lo, end.hi),
                    )
                }
                None => {
                    let cspan = self.file.expr_spans[rstart.0 as usize];
                    let callee = self.file.add_expr(
                        Expr::Member {
                            receiver: rstart,
                            name: "contains".to_string(),
                        },
                        Span::new(start.lo, cspan.hi),
                    );
                    let call = self.file.add_expr(
                        Expr::Call {
                            callee,
                            args: vec![subj],
                        },
                        Span::new(start.lo, cspan.hi),
                    );
                    if negated {
                        self.file.add_expr(
                            Expr::Unary {
                                op: UnOp::Not,
                                operand: call,
                            },
                            Span::new(start.lo, cspan.hi),
                        )
                    } else {
                        call
                    }
                }
            };
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
        self.file.add_expr(
            Expr::Block {
                stmts: vec![s],
                trailing: None,
            },
            Span::new(start.lo, end.hi),
        )
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
    // NOTE: `external` is deliberately excluded — ignoring it (no native body) would *miscompile*
    // rather than skip. `tailrec` IS recognized: the lowerer rewrites the tail self-calls into a loop
    // (so deep recursion doesn't overflow); a non-tail-optimizable `tailrec` falls back to plain
    // recursion (kotlinc warns; same runtime for the shallow cases).
    matches!(
        text,
        "public"
            | "private"
            | "internal"
            | "protected"
            | "open"
            | "final"
            | "abstract"
            | "inline"
            | "noinline"
            | "crossinline"
            | "operator"
            | "override"
            | "suspend"
            | "tailrec"
            | "lateinit"
            | "infix"
            | "reified"
            | "vararg"
            | "const"
            | "sealed"
            | "actual"
            | "expect"
            | "value"
            | "inner"
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
    let inner = raw
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw);
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
                u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .unwrap_or('\0')
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
    let text = text.trim_end_matches(['L', 'l', 'u', 'U']);
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
        u64::from_str_radix(digits, radix)
            .map(|v| v as i64)
            .unwrap_or(0)
    }
}

fn parse_unsigned_literal_bits(text: &str) -> u64 {
    let text = text.trim_end_matches(|c: char| matches!(c, 'L' | 'l' | 'u' | 'U'));
    let t: String = text.chars().filter(|c| *c != '_').collect();
    let (radix, digits) = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (16, h)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (2, b)
    } else {
        (10, t.as_str())
    };
    u64::from_str_radix(digits, radix).unwrap_or(0)
}

fn unquote(raw: &str) -> String {
    // Raw string `"""..."""`: content is verbatim (no escape processing), three quotes each side.
    if raw.starts_with("\"\"\"") {
        let inner = raw
            .strip_prefix("\"\"\"")
            .and_then(|s| s.strip_suffix("\"\"\""))
            .unwrap_or(raw);
        return inner.to_string();
    }
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
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
        assert!(
            !d.has_errors(),
            "unexpected parse errors: {}",
            d.render("test", src)
        );
        file.debug_tree()
    }

    #[test]
    fn simple_fun() {
        assert_eq!(
            tree("fun add(a: Int, b: Int): Int = a + b"),
            "(fun add (param a Int) (param b Int) :Int (+ a b))\n"
        );
    }

    /// Annotation capture (drives the compiler-extension surface): the parser records applied
    /// annotation simple names on a class, attached to the RIGHT declaration, excluding use-site ones.
    #[test]
    fn captures_class_annotations() {
        let mut d = DiagSink::new();
        let src = "@Serializable class Foo(val a: Int)\nclass Bar(val b: Int)";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "{}", d.render("test", src));

        let anns = |name: &str| {
            file.decl_arena
                .iter()
                .find_map(|decl| match decl {
                    Decl::Class(c) if c.name == name => Some(c.annotations.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("class {name} not found"))
        };
        assert_eq!(anns("Foo"), vec!["Serializable".to_string()]);
        assert!(
            anns("Bar").is_empty(),
            "annotation must not leak to the next class"
        );
    }

    #[test]
    fn use_site_annotations_excluded_from_capture() {
        // A use-site `@file:`/`@get:` target annotation doesn't apply to the declaration.
        let mut d = DiagSink::new();
        let src = "@kotlinx.serialization.Serializable\nclass Q(val a: Int)";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "{}", d.render("test", src));
        let q = file
            .decl_arena
            .iter()
            .find_map(|decl| match decl {
                Decl::Class(c) if c.name == "Q" => Some(c.annotations.clone()),
                _ => None,
            })
            .unwrap();
        // Fully-qualified annotation is captured by its SIMPLE name.
        assert_eq!(q, vec!["Serializable".to_string()]);
    }

    #[test]
    fn captures_function_annotations() {
        // Function annotations are captured on FunDecl (mirroring class capture) and don't leak.
        let mut d = DiagSink::new();
        let src = "@Composable fun A() {}\nfun B() {}";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "{}", d.render("test", src));

        let anns = |name: &str| {
            file.decl_arena
                .iter()
                .find_map(|decl| match decl {
                    Decl::Fun(f) if f.name == name => Some(f.annotations.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("fun {name} not found"))
        };
        assert_eq!(anns("A"), vec!["Composable".to_string()]);
        assert!(
            anns("B").is_empty(),
            "annotation must not leak to the next function"
        );
    }

    #[test]
    fn captures_parameter_annotations() {
        // Parameter annotations are captured on Param (e.g. Compose's `@IntroducedAt`), arguments
        // discarded, and don't leak to the next parameter.
        let mut d = DiagSink::new();
        let src = "fun f(a: Int, @IntroducedAt(\"1\") b: String = \"x\", c: Boolean) {}";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "{}", d.render("test", src));
        let params = file
            .decl_arena
            .iter()
            .find_map(|decl| match decl {
                Decl::Fun(f) if f.name == "f" => Some(f.params.clone()),
                _ => None,
            })
            .expect("fun f not found");
        assert!(params[0].annotations.is_empty(), "a: no annotations");
        assert_eq!(
            params[1].annotations,
            vec!["IntroducedAt".to_string()],
            "b: @IntroducedAt captured (arg discarded)"
        );
        assert!(
            params[2].annotations.is_empty(),
            "c: annotation must not leak from b"
        );
    }

    #[test]
    fn captures_annotations_on_a_function_type() {
        // `@Composable () -> Unit` (an annotated function TYPE) parses, and the annotation is recorded
        // by the type's start offset; an unannotated function-type param has none.
        let mut d = DiagSink::new();
        let src = "fun W(content: @Composable () -> Unit, plain: () -> Unit) {}";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "{}", d.render("test", src));

        let w = file
            .decl_arena
            .iter()
            .find_map(|decl| match decl {
                Decl::Fun(f) if f.name == "W" => Some(f),
                _ => None,
            })
            .expect("fun W");
        let ty_of = |name: &str| {
            &w.params
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("param {name}"))
                .ty
        };
        let anns_of = |name: &str| {
            file.type_annotations
                .get(&ty_of(name).span.lo)
                .cloned()
                .unwrap_or_default()
        };
        assert_eq!(anns_of("content"), vec!["Composable".to_string()]);
        assert!(
            anns_of("plain").is_empty(),
            "an unannotated function type has no recorded annotations"
        );
    }

    #[test]
    fn receiver_function_type_param() {
        // A receiver (extension) function type `Recv.() -> R` parses by folding the receiver in as the
        // first `FunctionN` parameter — no parse error (was "expected ')'" before).
        let mut d = DiagSink::new();
        let src = "fun build(instructions: Buildee<T>.(Int) -> Unit) {}";
        let toks = lex(src, &mut d);
        let _ = parse(src, &toks, &mut d);
        assert!(
            !d.has_errors(),
            "receiver function type should parse: {}",
            d.render("test", src)
        );
    }

    #[test]
    fn precedence_mul_over_add() {
        assert_eq!(
            tree("fun f(a: Int, b: Int, c: Int): Int = a + b * c"),
            "(fun f (param a Int) (param b Int) (param c Int) :Int (+ a (* b c)))\n"
        );
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
        assert_eq!(
            tree("fun f(a: Int, b: Int, c: Int): Int = a - b - c"),
            "(fun f (param a Int) (param b Int) (param c Int) :Int (- (- a b) c))\n"
        );
    }

    #[test]
    fn paren_overrides() {
        assert_eq!(
            tree("fun f(a: Int, b: Int, c: Int): Int = (a + b) * c"),
            "(fun f (param a Int) (param b Int) (param c Int) :Int (* (+ a b) c))\n"
        );
    }

    #[test]
    fn member_call() {
        assert_eq!(
            tree("fun f(a: Int, b: String): String = a.toString() + b"),
            "(fun f (param a Int) (param b String) :String (+ (call (. a toString)) b))\n"
        );
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
        assert_eq!(
            tree("fun f(a: Int, b: Int): Int = -a * b"),
            "(fun f (param a Int) (param b Int) :Int (* (neg a) b))\n"
        );
    }

    #[test]
    fn if_expr() {
        assert_eq!(
            tree("fun max(a: Int, b: Int): Int = if (a > b) a else b"),
            "(fun max (param a Int) (param b Int) :Int (if (> a b) a b))\n"
        );
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
        assert_eq!(
            tree("class Point(val x: Int, var y: String)"),
            "(class Point (val x Int) (var y String))\n"
        );
    }

    #[test]
    fn class_with_empty_body() {
        assert_eq!(
            tree("class Box(val v: Int) {\n}"),
            "(class Box (val v Int))\n"
        );
    }

    #[test]
    fn modifiers_and_annotations_are_skipped() {
        // Leading modifiers + annotations are ignored; the declaration parses normally.
        assert_eq!(tree("public inline fun f(): Int = 1"), "(fun f :Int 1)\n");
        assert_eq!(tree("@JvmStatic fun g(): Int = 2"), "(fun g :Int 2)\n");
        assert_eq!(
            tree("@Anno(1, 2) open class C(private val x: Int)"),
            "(class C (val x Int))\n"
        );
        // `data` is NOT a skippable modifier — it stays a data class.
        assert_eq!(tree("data class P(val x: Int)"), "(class P (val x Int))\n");
    }

    #[test]
    fn nullable_null_notnull_elvis() {
        assert_eq!(
            tree("fun f(s: String?): String = s ?: \"d\""),
            "(fun f (param s String) :String (?: s \"d\"))\n"
        );
        assert_eq!(
            tree("fun g(s: String?): String = s!!"),
            "(fun g (param s String) :String (!! s))\n"
        );
        assert_eq!(tree("fun h(): String = null"), "(fun h :String null)\n");
        // chained prefix `!` must NOT be confused with the postfix `!!` operator.
        assert_eq!(
            tree("fun n(p: Boolean): Boolean = !!!p"),
            "(fun n (param p Boolean) :Boolean (not (not (not p))))\n"
        );
    }

    #[test]
    fn for_loop_and_compound_assign() {
        let t = tree("fun f(n: Int): Int {\n var s = 0\n for (i in 1..n) s += i\n return s\n}");
        assert!(t.contains("(for i (1 .. n)"), "{t}");
        assert!(t.contains("(set s (+ s i))"), "{t}");
        // A range followed by `step` (an ordinary infix function — not special syntax) is iterated as
        // the progression VALUE it builds: `n downTo 0 step 2` → `(n.downTo(0)).step(2)`, a `for-each`.
        let g = tree("fun g(n: Int) {\n for (i in n downTo 0 step 2) {}\n}");
        assert!(
            g.contains("(for-each i (call (. (call (. n downTo) 0) step) 2)"),
            "{g}"
        );
        // Chained `step` chains the calls left-to-right (the second `step` is NOT swallowed as `2.step`).
        let c = tree("fun c(n: Int) {\n for (i in 0..6 step 2 step 3) {}\n}");
        assert!(
            c.contains("(call (. (call (. (.. 0 6) step) 2) step) 3)"),
            "{c}"
        );
        // A bare range keeps the optimized counted `for`.
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
        assert_eq!(
            tree("data class Point(val x: Int, val y: Int)"),
            "(class Point (val x Int) (val y Int))\n"
        );
        // `data` remains usable as an ordinary identifier.
        assert_eq!(
            tree("fun f(data: Int): Int = data"),
            "(fun f (param data Int) :Int data)\n"
        );
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
