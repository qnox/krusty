//! Stage B: tokens → arena AST. Recursive descent for decls/stmts, Pratt for expressions with the
//! Kotlin precedence table. Newlines (their own token) act as statement/expression terminators;
//! they are skipped after binary operators and between statements/declarations.

use crate::ast::*;
use crate::diag::{DiagSink, Span};
use crate::features::LangFeatures;
use crate::kt_string::{KtString, KtStringBuf};
use crate::token::{decode_char_literal_content, Token, TokenKind};
use crate::types::Visibility;

/// Parse with the default language feature set.
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
    parse_with_features_and_script(src, tokens, diags, features, false)
}

/// Parse a Kotlin script.
pub fn parse_script_with_features(
    src: &str,
    tokens: &[Token],
    diags: &mut DiagSink,
    features: &LangFeatures,
) -> File {
    parse_with_features_and_script(src, tokens, diags, features, true)
}

fn parse_with_features_and_script(
    src: &str,
    tokens: &[Token],
    diags: &mut DiagSink,
    features: &LangFeatures,
    is_script: bool,
) -> File {
    let mut p = Parser {
        src,
        t: tokens,
        i: 0,
        file: File::default(),
        diags,
        name_based_destructuring: features.has("NameBasedDestructuring"),
        short_form_destructuring: features.has("EnableNameBasedDestructuringShortForm"),
        multi_dollar_interpolation: features.has("MultiDollarInterpolation"),
        explicit_backing_fields: features.has("ExplicitBackingFields"),
        context_parameters: features.has("ContextParameters"),
        when_guards: features.has("WhenGuards"),
        no_trailing_lambda: false,
        block_trailing_is_value: false,
        lexical_type_params: Vec::new(),
        lexical_type_param_bounds: Vec::new(),
        pending_annotations: Vec::new(),
        pending_annotation_args: Vec::new(),
        pending_context_params: Vec::new(),
        is_script,
        script_stmts: Vec::new(),
        expr_depth: 0,
        type_depth: 0,
        stmt_depth: 0,
        parsing_anonymous_function_receiver: false,
    };
    p.file.is_script = is_script;
    // Run the parse with the recursion-bound stack reserve already in place (like the checker's
    // and lowering's pass entries): without this, the calling thread's remaining stack is
    // (almost) always under the reserve, so the FIRST `parse_bp` of every top-level expression
    // would allocate and free a fresh grown segment — per statement. With it, the per-level
    // checks in `parse_bp` are a stack-pointer read that only grows on genuinely deep nesting.
    crate::wide_stack::on_wide_stack(|| p.parse_file());
    if !p.script_stmts.is_empty() {
        let first = p.file.stmt_spans[p.script_stmts[0].0 as usize];
        let last = p.file.stmt_spans[p.script_stmts[p.script_stmts.len() - 1].0 as usize];
        let stmts = std::mem::take(&mut p.script_stmts);
        let body = p.file.add_expr(
            Expr::Block {
                stmts,
                trailing: None,
            },
            Span::new(first.lo, last.hi),
        );
        p.file.script_body = Some(body);
    }
    p.file.assert_always_enabled = features.has("AssertionsAlwaysEnable");
    p.file.assert_always_disabled = features.has("AssertionsAlwaysDisable");
    p.file.explicit_context_arguments = features.has("ExplicitContextArguments");
    p.file.implicit_signed_to_unsigned_integer_conversion =
        features.has("ImplicitSignedToUnsignedIntegerConversion");
    let script_scope = is_script.then(|| {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        src.hash(&mut hasher);
        hasher.finish()
    });
    hoist_local_classes(&mut p.file, script_scope);
    fixup_parenless_base_classes(&mut p.file);
    fill_class_decl_lines(&mut p.file, src);
    expand_fun_type_aliases(&mut p.file);
    p.file
}

/// Fill each `ClassDecl::decl_line` with the 1-based source line of its `span.lo`. kotlinc maps the
/// `LineNumberTable` of a class's synthesized members (primary ctor, property accessors) to the class
/// declaration line; the emitter reads this off `IrClass::decl_line`. Uses a single ascending scan of
/// newline offsets so the whole arena costs one pass over `src`.
fn fill_class_decl_lines(file: &mut File, src: &str) {
    // `line_starts[k]` = byte offset where line (k+1) begins; line 1 starts at 0.
    let mut line_starts = vec![0u32];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i as u32 + 1);
        }
    }
    file.source_line_count = line_starts.len() as u32;
    let line_at = |off: u32| -> u32 {
        // 1-based line: index of the last start <= off.
        match line_starts.binary_search(&off) {
            Ok(k) => k as u32 + 1,
            Err(k) => k as u32, // k = count of starts strictly <= off (since off not found)
        }
    };
    let span_line_at = |span: Span, off: u32| {
        if span.lo == 0 && span.hi == 0 {
            0
        } else {
            line_at(off)
        }
    };
    // Per-node start lines for the statement-level `LineNumberTable` mapping.
    file.expr_lines = file
        .expr_spans
        .iter()
        .map(|&span| span_line_at(span, span.lo))
        .collect();
    file.expr_source_lines = file
        .expr_arena
        .iter()
        .enumerate()
        .map(|(index, expr)| {
            let member_name_span = |index: usize, name: &str| {
                file.exact_member_name_spans
                    .get(&(index as u32))
                    .copied()
                    .unwrap_or_else(|| {
                        let span = file.expr_spans[index];
                        Span::new(span.hi.saturating_sub(name.len() as u32), span.hi)
                    })
            };
            let anchor = match expr {
                Expr::Call { callee, .. } => match &file.expr_arena[callee.0 as usize] {
                    Expr::Member { name, .. } | Expr::SafeCall { name, .. } => {
                        member_name_span(callee.0 as usize, name)
                    }
                    _ => file.expr_spans[callee.0 as usize],
                },
                Expr::Member { name, .. } | Expr::SafeCall { name, .. } => {
                    member_name_span(index, name)
                }
                _ => file.expr_spans[index],
            };
            span_line_at(anchor, anchor.lo)
        })
        .collect();
    file.expr_end_lines = file
        .expr_spans
        .iter()
        .map(|&span| span_line_at(span, span.hi))
        .collect();
    file.stmt_lines = file
        .stmt_spans
        .iter()
        .map(|&span| span_line_at(span, span.lo))
        .collect();
    // Snapshot each expression's start offset before the mutable walk of `decl_arena` (a disjoint
    // field, but only borrowck's field-splitting sees that — a helper can't).
    let expr_lo: Vec<u32> = file.expr_spans.iter().map(|s| s.lo).collect();
    let expr_hi: Vec<u32> = file.expr_spans.iter().map(|s| s.hi).collect();
    // kotlinc attributes a method's `LineNumberTable` to its BODY, not the `fun` keyword — for an
    // EXPRESSION body that is the `=` expression's line, which differs from the declaration line when
    // the signature wraps across lines. A block body is left at the declaration line (its per-statement
    // mapping is a separate, larger matter); a single-line expression body is unchanged (same line).
    let body_line = |body: &FunBody, decl: u32| -> u32 {
        match body {
            FunBody::Expr(e) => expr_lo.get(e.0 as usize).map_or(decl, |&lo| line_at(lo)),
            _ => decl,
        }
    };
    // A BLOCK body's closing `}` line — kotlinc maps a `Unit` fn's implicit `return` there.
    let body_close = |body: &FunBody| -> u32 {
        match body {
            FunBody::Block(e) => {
                // A synthesized block with the zero span stays "unknown" (mirror the
                // `expr_lines`/`stmt_lines` guard) — `line_at(0)` would fabricate line 1.
                let (lo, hi) = expr_lo
                    .get(e.0 as usize)
                    .copied()
                    .zip(expr_hi.get(e.0 as usize).copied())
                    .unwrap_or((0, 0));
                if lo == 0 && hi == 0 {
                    0
                } else {
                    line_at(hi.saturating_sub(1))
                }
            }
            _ => 0,
        }
    };
    for decl in &mut file.decl_arena {
        match decl {
            Decl::Class(c) => {
                c.decl_line = line_at(c.span.lo);
                // The parser stored the primary ctor's `)` OFFSET here — rewrite it to the line.
                if c.ctor_close_line != 0 {
                    c.ctor_close_line = line_at(c.ctor_close_line);
                }
                // A class's methods live INSIDE the class decl, not in `decl_arena` — walk them too,
                // or every member method keeps line 0 and gets no `LineNumberTable`.
                for m in &mut c.methods {
                    m.decl_line = body_line(&m.body, line_at(m.span.lo));
                    m.body_close_line = body_close(&m.body);
                }
                for p in &mut c.props {
                    if p.span.lo != 0 || p.span.hi != 0 {
                        p.decl_line = line_at(p.span.lo);
                    }
                }
                for p in &mut c.body_props {
                    p.decl_line = line_at(p.span.lo);
                }
                for e in &mut c.enum_entries {
                    e.decl_line = line_at(e.span.lo);
                }
            }
            Decl::Fun(f) => {
                f.decl_line = body_line(&f.body, line_at(f.span.lo));
                f.body_close_line = body_close(&f.body);
            }
            _ => {}
        }
    }
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

fn modality_from_modifiers(modifiers: &[String]) -> crate::ast::Modality {
    let sealed = modifiers.iter().any(|modifier| modifier == "sealed");
    modality_of(
        sealed || modifiers.iter().any(|modifier| modifier == "open"),
        sealed || modifiers.iter().any(|modifier| modifier == "abstract"),
        sealed,
    )
}

/// The degraded result of a tripped declaration-nesting guard: an empty final class named
/// `<error>`. That source-impossible synthetic name cannot collide with a declared class, and the
/// empty body gives the later passes nothing to recurse over.
fn error_class_decl(span: crate::diag::Span) -> ClassDecl {
    ClassDecl {
        primary_ctor_visibility: Visibility::Public,
        name: "<error>".to_string(),
        visibility: Visibility::Public,
        annotations: Vec::new(),
        annotation_args: Vec::new(),
        type_parameters: crate::ast::ClassTypeParameters::new(Vec::new(), Vec::new(), Vec::new()),
        props: Vec::new(),
        methods: Vec::new(),
        companion: None,
        body_props: Vec::new(),
        init_order: Vec::new(),
        is_data: false,
        is_value: false,
        kind: ClassKind::Class,
        singleton: false,
        enum_entries: Vec::new(),
        is_fun_interface: false,
        modality: crate::ast::Modality::Final,
        inner_of: None,
        supertypes: Vec::new(),
        delegations: Vec::new(),
        delegation_exprs: Vec::new(),
        base_class: None,
        base_type_args: Vec::new(),
        base_args: Vec::new(),
        primary_ctor_annotations: Some(Vec::new()),
        secondary_ctors: Vec::new(),
        span,
        ctor_close_line: 0,
        decl_line: 0,
    }
}

/// A non-nullable, non-generic type reference.
fn simple_type_ref(name: &str, span: crate::diag::Span) -> TypeRef {
    TypeRef {
        name: name.to_string(),
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

/// Hoist every local class (`class`/`interface`/… declared inside a function body, parsed as
/// `Stmt::LocalClass`) to a top-level `Decl::Class`, so signature collection and lowering treat it
/// like any other class. The `Stmt::LocalClass` stays in the body and is where the CHECKER enters
/// the class (`File::local_class_decls` links the two), so it is checked in the lexical scope it
/// was written in.
///
/// The hoisted declaration is named after the declaration it was written in, as a DOTTED path
/// (`box.Local`, `Outer.m.Local`) — the same spelling a nested class uses, so `class_internal` turns
/// it into `Outer$m$Local` and every lexical-prefix walk already in the compiler finds it without
/// being taught anything new. It is also how kotlinc names a local class. Nothing in the source is
/// rewritten to match: the SOURCE name is what the checker binds in `Ns::Classifier`, so a reference
/// resolves where it was written.
fn hoist_local_classes(file: &mut File, script_scope: Option<u64>) {
    use crate::ast::{Decl, FunBody, Stmt, StmtId};
    let mut owners: Vec<(StmtId, String)> = Vec::new();
    let record = |file: &File, body: &FunBody, owner: String, out: &mut Vec<(StmtId, String)>| {
        let (FunBody::Expr(root) | FunBody::Block(root)) = body else {
            return;
        };
        for stmt in local_class_stmts(file, *root) {
            out.push((stmt, owner.clone()));
        }
    };
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) => record(file, &f.body, f.name.clone(), &mut owners),
            Decl::Class(c) => {
                for m in &c.methods {
                    record(file, &m.body, format!("{}.{}", c.name, m.name), &mut owners);
                }
                // A class body's `init` blocks and property initializers run in the constructor;
                // kotlinc names a local class declared there after the CLASS.
                for step in &c.init_order {
                    if let crate::ast::ClassInit::Block(b) = step {
                        record(file, &FunBody::Block(*b), c.name.clone(), &mut owners);
                    }
                }
                for p in &c.body_props {
                    if let Some(init) = p.init {
                        record(file, &FunBody::Expr(init), c.name.clone(), &mut owners);
                    }
                }
                // A primary-constructor parameter default is evaluated in the constructor too.
                for d in c.props.iter().filter_map(|p| p.default) {
                    record(file, &FunBody::Expr(d), c.name.clone(), &mut owners);
                }
            }
            Decl::Property(p) => {
                if let Some(init) = p.init {
                    record(file, &FunBody::Expr(init), p.name.clone(), &mut owners);
                }
            }
        }
    }
    if let Some(root) = file.script_body {
        // A script re-parses each snippet into the same facade, so the snippet identity separates
        // two local classes that would otherwise share a name and an (empty) owner.
        let owner = format!("script${:x}", script_scope.unwrap_or_default());
        for stmt in local_class_stmts(file, root) {
            owners.push((stmt, owner.clone()));
        }
    }
    let owner_of: std::collections::HashMap<StmtId, String> = owners.into_iter().collect();
    let hoisted: Vec<(StmtId, crate::ast::ClassDecl)> = file
        .stmt_arena
        .iter()
        .enumerate()
        .filter_map(|(index, s)| match s {
            Stmt::LocalClass(c) => Some((StmtId(index as u32), c.clone())),
            _ => None,
        })
        .collect();
    // Two local classes of the same name under the same owner (declared in sibling blocks) still
    // need telling apart; the statement index is unique by construction.
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (stmt, mut c) in hoisted {
        let mut name = match owner_of.get(&stmt) {
            Some(owner) => format!("{owner}.{}", c.name),
            None => format!("loc{}.{}", stmt.0, c.name),
        };
        if !taken.insert(name.clone()) {
            name = format!("{name}{}", stmt.0);
            taken.insert(name.clone());
        }
        // A nested type hoisted out of this local class was named from the local class's SOURCE
        // name (`Local.Inner`); requalifying the outer without it would orphan the nested one.
        let qualifier = name.strip_suffix(&c.name).map(str::to_string);
        if let (Some(qualifier), Some(nested)) =
            (qualifier, file.local_class_nested.get(&stmt).cloned())
        {
            let source = c.name.clone();
            for did in nested {
                if let Decl::Class(nc) = file.decl_mut(did) {
                    nc.name = format!("{qualifier}{}", nc.name);
                    // An `inner class` names its enclosing class by the same path, and its
                    // synthetic outer-instance field is typed from it.
                    if let Some(outer) = nc.inner_of.as_deref() {
                        if outer == source || outer.starts_with(&format!("{source}.")) {
                            nc.inner_of = Some(format!("{qualifier}{outer}"));
                        }
                    }
                }
            }
        }
        c.name = name;
        let id = file.add_decl(Decl::Class(c));
        file.decls.push(id);
        file.mark_local_declaration(id);
        file.local_class_decls.insert(stmt, id);
    }
}

/// The `Stmt::LocalClass` statements reachable from `root`, including those inside the member bodies
/// of a local class declared there (`any_child_stmt` stops at the local class, but its members are
/// written in the same lexical scope).
fn local_class_stmts(file: &File, root: ExprId) -> Vec<StmtId> {
    use crate::ast::{FunBody, Stmt};
    let mut exprs: Vec<ExprId> = vec![root];
    let mut stmts: Vec<StmtId> = Vec::new();
    let mut out: Vec<StmtId> = Vec::new();
    loop {
        if let Some(e) = exprs.pop() {
            file.any_child_expr(
                e,
                &mut |child| {
                    exprs.push(child);
                    false
                },
                &mut |child| {
                    stmts.push(child);
                    false
                },
            );
        } else if let Some(s) = stmts.pop() {
            file.any_child_stmt(s, &mut |child| {
                exprs.push(child);
                false
            });
            if let Stmt::LocalClass(c) = file.stmt(s) {
                out.push(s);
                for m in &c.methods {
                    if let FunBody::Expr(e) | FunBody::Block(e) = m.body {
                        exprs.push(e);
                    }
                }
            }
        } else {
            break;
        }
    }
    out
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
    let mut detached = Vec::new();
    for d in file.decl_arena.iter_mut() {
        if let Decl::Class(c) = d {
            if c.primary_ctor_annotations.is_some() || c.base_class.is_some() {
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
                .position(|s| base_candidates.contains(&s.name))
            {
                let base = c.supertypes.remove(pos);
                detached.push(base.clone());
                c.base_class = Some(base.name);
                c.base_type_args = base.targs;
            }
        }
    }
    file.detached_type_refs.extend(detached);
}

/// Expand the file's FUNCTION-type aliases (`typealias L = (A) -> R`) structurally: every `TypeRef`
/// naming such an alias is rewritten to the target function type, so all downstream consumers (the
/// checker's raw-`TypeRef` function-type tests, the lowerer, metadata) see the ordinary arrow form.
/// Runs per file at the parse seam — an alias declared in a SIBLING file is not expanded (unresolved
/// → the test skips, as before). Nested alias references inside a substituted target expand through
/// the recursion; the depth cap only guards a (kotlinc-rejected) recursive alias from looping.
fn expand_fun_type_aliases(file: &mut File) {
    use std::collections::HashMap;
    if file.type_alias_fun.is_empty() {
        return;
    }
    let aliases: HashMap<String, (Vec<String>, TypeRef)> = file
        .type_alias_fun
        .iter()
        .map(|(n, ps, t)| (n.clone(), (ps.clone(), t.clone())))
        .collect();

    /// Replace every leaf reference to one of `params` in `tr` with the corresponding use-site type
    /// argument (the use site's `?` on the reference survives onto the substituted argument).
    fn substitute(tr: &mut TypeRef, params: &[String], args: &[TypeRef]) {
        if tr.fun_params.is_empty() && tr.name != "<fun>" && tr.targs.is_empty() {
            if let Some(k) = params.iter().position(|p| *p == tr.name) {
                let nullable = tr.nullable();
                let span = tr.span;
                *tr = args[k].clone();
                tr.set_nullable(tr.nullable() || nullable);
                tr.span = span;
                return; // a use-site type argument carries no alias type parameters of its own
            }
        }
        if let Some(a) = &mut tr.arg {
            substitute(a, params, args);
        }
        for t in &mut tr.targs {
            substitute(t, params, args);
        }
        for t in &mut tr.fun_params {
            substitute(t, params, args);
        }
    }

    fn expand(tr: &mut TypeRef, aliases: &HashMap<String, (Vec<String>, TypeRef)>, depth: u32) {
        if depth > 8 {
            return;
        }
        if tr.fun_params.is_empty() && tr.name != "<fun>" {
            if let Some((tparams, target)) = aliases.get(&tr.name) {
                if tr.targs.len() == tparams.len() {
                    // Expand any alias reference INSIDE the use-site type arguments first — the
                    // substitution clones them into the target, after which the originals are gone.
                    for a in &mut tr.targs {
                        expand(a, aliases, depth + 1);
                    }
                    let nullable = tr.nullable();
                    let span = tr.span;
                    let mut expanded = target.clone();
                    if !tparams.is_empty() {
                        substitute(&mut expanded, tparams, &tr.targs);
                    }
                    *tr = expanded;
                    // The use site's `?` survives (`Listener?` = nullable function type); the span
                    // stays the USE site so diagnostics point at the source that wrote it.
                    tr.set_nullable(tr.nullable() || nullable);
                    tr.span = span;
                    // The expansion (or a substitution result) may itself be an alias reference
                    // (`typealias A<T> = B<T>` with `B` a fn-type alias) — re-expand the whole node;
                    // the depth cap bounds a (kotlinc-rejected) recursive chain.
                    expand(tr, aliases, depth + 1);
                    return;
                }
            }
        }
        if let Some(a) = &mut tr.arg {
            expand(a, aliases, depth + 1);
        }
        for t in &mut tr.targs {
            expand(t, aliases, depth + 1);
        }
        for t in &mut tr.fun_params {
            expand(t, aliases, depth + 1);
        }
    }
    fn expand_opt(tr: &mut Option<TypeRef>, aliases: &HashMap<String, (Vec<String>, TypeRef)>) {
        if let Some(t) = tr {
            expand(t, aliases, 0);
        }
    }
    fn expand_params(params: &mut [Param], aliases: &HashMap<String, (Vec<String>, TypeRef)>) {
        for p in params {
            expand(&mut p.ty, aliases, 0);
        }
    }
    fn expand_bounds(
        bounds: &mut [(String, TypeRef)],
        aliases: &HashMap<String, (Vec<String>, TypeRef)>,
    ) {
        for (_, t) in bounds {
            expand(t, aliases, 0);
        }
    }
    fn expand_fun(f: &mut FunDecl, aliases: &HashMap<String, (Vec<String>, TypeRef)>) {
        expand_opt(&mut f.receiver, aliases);
        expand_params(&mut f.params, aliases);
        expand_opt(&mut f.ret, aliases);
        expand_bounds(&mut f.type_param_bounds, aliases);
    }
    fn expand_prop(p: &mut PropDecl, aliases: &HashMap<String, (Vec<String>, TypeRef)>) {
        expand_opt(&mut p.receiver, aliases);
        expand_opt(&mut p.ty, aliases);
        expand_bounds(&mut p.type_param_bounds, aliases);
    }
    fn expand_class(c: &mut ClassDecl, aliases: &HashMap<String, (Vec<String>, TypeRef)>) {
        c.type_parameters
            .map_bounds(|bounds| expand_bounds(bounds, aliases));
        for p in &mut c.props {
            expand(&mut p.ty, aliases, 0);
        }
        for t in &mut c.supertypes {
            expand(t, aliases, 0);
        }
        for m in &mut c.methods {
            expand_fun(m, aliases);
        }
        for p in &mut c.body_props {
            expand_prop(p, aliases);
        }
        for sc in &mut c.secondary_ctors {
            expand_params(&mut sc.params, aliases);
        }
        for e in &mut c.enum_entries {
            for m in &mut e.methods {
                expand_fun(m, aliases);
            }
            for p in &mut e.props {
                expand_prop(p, aliases);
            }
        }
    }

    let aliases = &aliases;
    for decl in &mut file.decl_arena {
        match decl {
            Decl::Fun(f) => expand_fun(f, aliases),
            Decl::Property(p) => expand_prop(p, aliases),
            Decl::Class(c) => expand_class(c, aliases),
        }
    }
    for expr in &mut file.expr_arena {
        match expr {
            Expr::Is { ty, .. } | Expr::As { ty, .. } => expand(ty, aliases, 0),
            Expr::Try { catches, .. } => {
                for c in catches {
                    expand(&mut c.ty, aliases, 0);
                }
            }
            _ => {}
        }
    }
    for stmt in &mut file.stmt_arena {
        match stmt {
            Stmt::Local { ty, .. } | Stmt::LocalDelegate { ty, .. } => expand_opt(ty, aliases),
            Stmt::LocalLateinit { ty, .. } => expand(ty, aliases, 0),
            Stmt::LocalFun(f) => expand_fun(f, aliases),
            Stmt::LocalClass(c) => expand_class(c, aliases),
            _ => {}
        }
    }
    for trs in file.call_type_args.values_mut() {
        for t in trs {
            expand(t, aliases, 0);
        }
    }
    for trs in file.lambda_param_types.values_mut() {
        for t in trs.iter_mut().flatten() {
            expand(t, aliases, 0);
        }
    }
    for t in file.anon_fun_ret.values_mut() {
        expand(t, aliases, 0);
    }
    for t in file.anon_fun_receivers.values_mut() {
        expand(t, aliases, 0);
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
    explicit_backing_fields: bool,
    /// `+EnableNameBasedDestructuringShortForm`: a plain paren entry `(a, b)` binds each variable to
    /// the RECEIVER PROPERTY of the same name (not `componentN`). An explicit `(a = prop)` still
    /// renames. Bracket `[a, b]` stays positional.
    short_form_destructuring: bool,
    multi_dollar_interpolation: bool,
    context_parameters: bool,
    when_guards: bool,
    /// When set, `parse_postfix` does NOT attach a trailing `{ … }` to a call as a lambda argument —
    /// used where a following `{` belongs to an enclosing construct (a `: I by Impl()` delegate, whose
    /// `{` opens the class body, not a lambda on the delegate call).
    no_trailing_lambda: bool,
    /// Whether the statement list currently being parsed belongs to a block whose trailing
    /// expression is semantically consumed (lambda and `if`/`when`/`try` value blocks). Function,
    /// accessor, constructor, init, loop, and `finally` blocks discard a trailing expression; they
    /// must keep inc/dec on the statement path, especially for an implicit-receiver property name.
    /// Saved/restored at every nested block boundary so syntax context, not a lambda-only special
    /// case, owns the decision.
    block_trailing_is_value: bool,
    /// Type parameters in the current lexical parser context. Synthetic anonymous classes are hoisted
    /// to file-level declarations, so they must carry the generic names they mention in supertypes or
    /// member signatures; otherwise checking the hoisted class reports `T` as unresolved.
    lexical_type_params: Vec<String>,
    lexical_type_param_bounds: Vec<(String, TypeRef)>,
    /// Simple names of annotations consumed by the most recent `skip_decl_prefix`, awaiting attachment
    /// to the declaration that follows (e.g. `@Serializable` → `["Serializable"]`). A `parse_X` reads
    /// it via `take_pending_annotations()` *before* parsing members (member prefixes overwrite it).
    pending_annotations: Vec<AnnotationRef>,
    /// The argument expressions of each pending annotation (parallel to `pending_annotations`). Only the
    /// direct, ordinary-expression args are kept (array/nested-annotation args record an empty vec), which
    /// is all an extension reading a value annotation (`@SerialName("x")`) needs.
    pending_annotation_args: Vec<Vec<ExprId>>,
    /// Context parameters parsed at a declaration prefix (`context(a: A)`), consumed by the next
    /// `parse_fun` (mirrors `pending_annotations`). Cleared when taken.
    pending_context_params: Vec<Param>,
    is_script: bool,
    script_stmts: Vec<StmtId>,
    /// Current expression-recursion depth (see [`Parser::parse_bp`]). Bounded by
    /// [`EXPR_DEPTH_LIMIT`] so a pathologically nested expression (`((((…x…))))`) degrades to an
    /// error expression instead of overflowing the stack — the same "degrade, never crash"
    /// contract as the checker's and lowering's `expr_depth` guards.
    expr_depth: u32,
    /// Current TYPE-recursion depth (see [`Parser::parse_type`]). Types recurse outside `parse_bp`
    /// (declaration position: `val x: ((((Int))))`, `List<List<…>>`), so they need their own
    /// bounded, stack-grown funnel with the same degrade-never-crash contract.
    type_depth: u32,
    /// Current statement/declaration structural-recursion depth (see [`Parser::parse_stmt`] and
    /// the class-like declaration parsers). Nested blocks (`while { while { … } }`) and nested
    /// type declarations (`class A { class B { … } }`) recurse outside `parse_bp` too; one shared
    /// counter bounds any interleaving of the two shapes.
    stmt_depth: u32,
    /// In `fun R.(...)`, `.(` terminates the receiver instead of beginning the function-type
    /// grammar `R.(...) -> T`.
    parsing_anonymous_function_receiver: bool,
}

/// Semantic parser-recursion funnels governed by one depth/recovery mechanism.
///
/// Statement and declaration entries intentionally select the same counter: a local class inside
/// a loop, or a loop inside an initializer of a local class, must not evade the bound by alternating
/// declaration origins. Type recovery alone treats angle brackets as delimiters because only type
/// grammar gives `<`/`>` that unconditional structural meaning.
#[derive(Clone, Copy)]
enum ParserNesting {
    Expression,
    Type,
    Statement,
    Declaration,
}

impl ParserNesting {
    fn label(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Type => "type",
            Self::Statement => "statement",
            Self::Declaration => "declaration",
        }
    }

    fn counter_mut<'parser, 'source>(
        self,
        parser: &'parser mut Parser<'source>,
    ) -> &'parser mut u32 {
        match self {
            Self::Expression => &mut parser.expr_depth,
            Self::Type => &mut parser.type_depth,
            Self::Statement | Self::Declaration => &mut parser.stmt_depth,
        }
    }

    fn angle_brackets(self) -> bool {
        matches!(self, Self::Type)
    }
}

/// Maximum expression-parse recursion depth, counted in `parse_bp` ENTRIES — not semantic nesting
/// levels. One semantic level can cost up to two entries (a binary operator's right operand is one
/// `parse_bp` recursion, and a parenthesized operand re-enters via `parse_expr` → another), so a
/// 450-level `0+(0+(…))` chain — which the checker and lowering admit under their 500-level
/// `expr_depth` guards — consumes ~900 entries. Twice the shared semantic bound therefore admits
/// every shape the later passes admit at up to two entries per level (redundant nesting like
/// doubled parens spends entries faster and trips sooner), while still bounding the parser's
/// recursion (degrade with a diagnostic, never crash). Derive this value rather than copying 1000
/// so parser/checker/lowering cannot silently drift to incompatible limits.
///
/// The type and statement/declaration funnels (`parse_type`, `parse_stmt`, the class-like
/// declaration parsers) reuse this limit as a conservative shared bound: their shapes cost one
/// entry per nesting level, so 1000 admits strictly deeper nesting there than any later pass
/// admits for expressions — the guard exists to bound the stack, not to define type/statement
/// acceptance.
const EXPR_DEPTH_LIMIT: u32 = crate::wide_stack::MAX_SEMANTIC_EXPR_DEPTH * 2;

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
    /// At the start of a named argument `name = value`: an identifier followed by a single `=` (NOT
    /// `==`, which the lexer produces as one `EqEq` token and which begins an equality expression).
    fn at_named_arg(&self) -> bool {
        self.at(TokenKind::Ident)
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::Eq)
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
    fn eat_span(&mut self, k: TokenKind) -> Option<Span> {
        self.at(k).then(|| self.bump().span)
    }
    /// The lexer exposes an identifier's decoded-content span. Extend it to the raw syntactic token for
    /// backticked identifiers while the parser still has source access; no source string is retained.
    fn syntactic_ident_span(&self, token: Token) -> Span {
        let bytes = self.src.as_bytes();
        let lo = token.span.lo as usize;
        let hi = token.span.hi as usize;
        if token.kind == TokenKind::Ident
            && lo > 0
            && hi < bytes.len()
            && bytes[lo - 1] == b'`'
            && bytes[hi] == b'`'
        {
            Span::new(token.span.lo - 1, token.span.hi + 1)
        } else {
            token.span
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
    /// True when the current token is a `;` (the lexer emits it as a `Newline`, but its source text is
    /// `";"` — distinguishing an explicit statement terminator from a plain line break). Used to detect
    /// an EMPTY loop body (`while (c);`), which must not swallow the following statement as the body.
    fn at_semicolon(&self) -> bool {
        self.at(TokenKind::Newline) && self.text() == ";"
    }
    /// Parse a loop body after the header's closing `)`. An explicit `;` is an EMPTY body (`while (c);`,
    /// `for (…);`) — consumed here so the following statement is NOT mistaken for the body; otherwise the
    /// body is the next statement/expression (possibly on the next line, so plain line breaks are skipped
    /// first). `parse_branch` handles a bare statement body (`while (c) i++`), not just an expression.
    fn parse_loop_body(&mut self) -> ExprId {
        if self.at_semicolon() {
            let sp = self.tok().span;
            self.bump();
            self.file.add_expr(
                Expr::Block {
                    stmts: Vec::new(),
                    trailing: None,
                },
                sp,
            )
        } else {
            self.skip_newlines();
            self.parse_branch(false)
        }
    }
    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.bump();
        }
    }

    /// Return the first token after physical line breaks at `index`, stopping at an explicit `;`.
    ///
    /// The lexer deliberately represents both forms as `Newline`, so grammar lookahead must use this
    /// helper instead of open-coding a token-kind loop whenever a construct permits `NL*` but not a
    /// semicolon. Keeping that distinction here prevents declaration callers from gradually acquiring
    /// different continuation rules.
    fn after_plain_newlines(&self, mut index: usize) -> usize {
        while self
            .t
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Newline && token.text(self.src) != ";")
        {
            index += 1;
        }
        index
    }

    /// Advance over physical line breaks while preserving an explicit semicolon as a declaration
    /// boundary. Mutating callers share the same lexer distinction as lookahead callers through
    /// `after_plain_newlines`; there must not be a second newline-scanning implementation here.
    fn skip_plain_newlines(&mut self) {
        self.i = self.after_plain_newlines(self.i);
    }

    /// Consume physical line breaks only when `expected` follows them. A failed lookahead leaves the
    /// parser at the original newline so the enclosing declaration can terminate normally.
    fn skip_plain_newlines_before(&mut self, expected: TokenKind) {
        let next = self.after_plain_newlines(self.i);
        if self.t.get(next).is_some_and(|token| token.kind == expected) {
            self.i = next;
        }
    }

    /// Kotlin treats a newline before `||`, `&&`, or `?:` as a line CONTINUATION, not a statement
    /// terminator (`cond\n  && other`, `x\n  ?: default`): the operator cannot begin a statement, so
    /// the expression keeps going. Peek past the newline(s); if such an operator follows, consume them
    /// so the binary/elvis loop sees the operator on the same logical line. (`+`/`-`/`*` deliberately do
    /// NOT continue — kotlinc parses a leading `-x` as a fresh unary-prefix statement, per the grammar
    /// which allows `NL*` before `||`/`&&`/elvis but not before the additive/multiplicative operators.)
    fn skip_newlines_before_continuation_op(&mut self) {
        if !self.at(TokenKind::Newline) {
            return;
        }
        let mut j = self.i;
        while self.t.get(j).is_some_and(|t| t.kind == TokenKind::Newline) {
            j += 1;
        }
        let continues = match self.t.get(j).map(|t| t.kind) {
            Some(TokenKind::OrOr | TokenKind::AndAnd) => true,
            Some(TokenKind::Question) => self
                .t
                .get(j + 1)
                .is_some_and(|t| t.kind == TokenKind::Colon),
            _ => false,
        };
        if continues {
            self.skip_newlines();
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
            self.pending_context_params.clear();
            if self.is_script && self.at_file_annotation() {
                if !self.script_stmts.is_empty() {
                    self.diags.error(
                        self.tok().span,
                        "file annotations must precede script statements",
                    );
                }
                self.skip_annotation();
                continue;
            }
            let script_file_item =
                matches!(self.kind(), TokenKind::KwPackage | TokenKind::KwImport)
                    || (self.at(TokenKind::Ident) && self.text() == "typealias");
            if self.is_script && script_file_item && !self.script_stmts.is_empty() {
                self.diags.error(
                    self.tok().span,
                    "package, import, and typealias directives must precede script statements",
                );
            }
            if self.is_script && !script_file_item {
                let stmt = self.parse_stmt();
                self.script_stmts.push(stmt);
                continue;
            }
            // Consume leading annotations + declaration modifiers. `open`/`abstract` are applied to
            // the following class; the rest are ignored (krusty treats everything as public).
            let mut mods = if self.at(TokenKind::At)
                || (self.at(TokenKind::Ident) && is_modifier(self.text()))
            {
                let m = self.skip_decl_prefix();
                self.skip_newlines();
                m
            } else {
                Vec::new()
            };
            // `context` is a soft keyword and only starts a clause before a declaration.
            mods.extend(self.maybe_parse_context_receivers());
            // A `sealed` class is implicitly abstract and open (subclasses live in the same module).
            let is_sealed = mods.iter().any(|m| m == "sealed");
            // `expect` (multiplatform header): whatever declaration the arm below pushes is
            // recorded so expect/actual matching can drop it once an `actual` provides the body.
            let is_expect = mods.iter().any(|m| m == "expect");
            let decls_before = self.file.decls.len();
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
                    let import_span = self.tok().span;
                    let mut fq = self.parse_qualified_name();
                    // `import a.b.*` — `parse_qualified_name` consumes the trailing `.` (it only keeps a
                    // segment when an `Ident` follows), leaving us at `*`. Recover the wildcard so it is
                    // recorded as `a.b.*` (the form the import-level builder recognizes).
                    if self.at(TokenKind::Star) {
                        self.bump(); // '*'
                        fq.push_str(".*");
                    }
                    let mut alias = None;
                    if self.at(TokenKind::Ident) && self.text() == "as" {
                        self.bump(); // 'as'
                        if self.at(TokenKind::Ident) {
                            alias = Some(self.text().to_string());
                            self.bump();
                        }
                    }
                    if !fq.is_empty() {
                        if !fq.ends_with(".*") {
                            let mut reference = simple_type_ref(&fq, import_span);
                            reference.flags = reference.flags.with_import(true);
                            self.file.detached_type_refs.push(reference);
                        }
                        if let Some(alias) = alias {
                            self.file.import_aliases.push((alias, fq.clone()));
                        }
                        self.file.imports.push(fq);
                    }
                    // tolerate any remaining trailing tokens to end of line
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
                    self.file.decls.insert(decls_before, id);
                }
                TokenKind::KwFun => {
                    let d = self.parse_fun(&mods);
                    let id = self.file.add_decl(Decl::Fun(d));
                    self.file.decls.push(id);
                }
                TokenKind::KwClass => {
                    let is_value = mods.iter().any(|m| m == "inline" || m == "value");
                    let mut d = self.parse_class();
                    d.modality = modality_from_modifiers(&mods);
                    d.is_value = is_value;
                    d.visibility = visibility_of(&mods);
                    let id = self.file.add_decl(Decl::Class(d));
                    self.file.decls.insert(decls_before, id);
                }
                // top-level property: `val`/`var name (: Type)? = init`
                TokenKind::KwVal | TokenKind::KwVar => {
                    // An `expect val/var` is a HEADER — legally initializer- and accessor-less
                    // (the matched actual supplies them; expect/actual stripping removes it).
                    let mut d = self.parse_top_property_c(
                        mods.iter().any(|m| m == "lateinit"),
                        is_expect,
                        mods.iter().any(|m| m == "const"),
                        false,
                    );
                    d.visibility = visibility_of(&mods);
                    d.is_override = mods.iter().any(|m| m == "override");
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
                    self.file.decls.insert(decls_before, id);
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
                    self.file.decls.insert(decls_before, id);
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
                    self.file.decls.insert(decls_before, id);
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
                    self.file.decls.insert(decls_before, id);
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
                    self.file.decls.insert(decls_before, id);
                }
                // `typealias Name[<T,...>] = Type`
                TokenKind::Ident if self.text() == "typealias" => {
                    self.bump(); // `typealias`
                    let alias = if self.at(TokenKind::Ident) {
                        self.bump().text(self.src).to_string()
                    } else {
                        String::new()
                    };
                    let alias_targs = self.parse_type_args(); // `<T, R>` if present
                    self.eat(TokenKind::Eq);
                    // A function-type target (`(A) -> R`, `suspend (A) -> R`, `context(C) (A) -> R`,
                    // a receiver form `R.() -> T`): parse the full type and record it in
                    // `type_alias_fun`. Every function-type spelling — and no class-name target —
                    // carries an `->` before the end of the line, so detect by scanning ahead. A
                    // GENERIC alias records its declared type-parameter NAMES (each `<T, R>` entry
                    // parses as a simple type) so the expansion pass substitutes use-site arguments.
                    let fn_target = self.t[self.i..]
                        .iter()
                        .take_while(|t| t.kind != TokenKind::Newline && t.kind != TokenKind::Eof)
                        .any(|t| t.kind == TokenKind::Arrow);
                    // Parse the target type name, including dotted FQNs (e.g. java.lang.Exception).
                    let target = if fn_target {
                        let t = self.parse_type();
                        // Every declared type parameter must be a SIMPLE name (no bounds/variance are
                        // parsed into `parse_type_args` results beyond the name; a non-simple entry —
                        // e.g. a `*` projection — declines recording).
                        let tparam_names: Option<Vec<String>> = alias_targs
                            .iter()
                            .map(|a| {
                                (a.fun_params.is_empty()
                                    && a.targs.is_empty()
                                    && !a.name.is_empty()
                                    && a.name != "<fun>"
                                    && a.name != "*")
                                    .then(|| a.name.clone())
                            })
                            .collect();
                        let is_fun = !t.fun_params.is_empty() || t.name == "<fun>";
                        if let Some(tparam_names) = tparam_names {
                            // A class target can contain a function type argument. It still needs
                            // structural expansion so those arguments survive (`Handlers` must mean
                            // `Map<String, (Int) -> Int>`, not raw `Map`); the presence of the nested
                            // arrow must not classify the alias itself as a function type.
                            if !alias.is_empty() && (is_fun || !t.targs.is_empty()) {
                                self.file.type_alias_fun.push((
                                    alias.clone(),
                                    tparam_names,
                                    t.clone(),
                                ));
                            }
                        }
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                        if is_fun {
                            String::new()
                        } else {
                            // The Arrow that triggered the scan sat inside a CLASS target's type
                            // argument (`Map<String, (Int) -> Int>`) — keep the class-name alias
                            // exactly as the plain-name path records it.
                            t.name.clone()
                        }
                    } else if self.at(TokenKind::Ident) {
                        // Parse the target as a full type — a dotted FQN and its type ARGUMENTS alike.
                        // Discarding the arguments made `typealias IntList = List<Int>` an alias for a
                        // raw `List`, so `for (x in xs)` handed back the erased bound.
                        let t = self.parse_type();
                        let tparam_names: Option<Vec<String>> = alias_targs
                            .iter()
                            .map(|a| {
                                (a.fun_params.is_empty()
                                    && a.targs.is_empty()
                                    && !a.name.is_empty()
                                    && a.name != "<fun>"
                                    && a.name != "*")
                                    .then(|| a.name.clone())
                            })
                            .collect();
                        // An alias whose target carries type arguments expands STRUCTURALLY, through
                        // the same pass and substitution the function-type aliases use. A bare
                        // `typealias A = Foo` keeps the name map, which the constructor-alias
                        // registration and the classifier lookups are keyed by.
                        if let Some(tparam_names) = tparam_names {
                            if !alias.is_empty() && !t.targs.is_empty() {
                                self.file.type_alias_fun.push((
                                    alias.clone(),
                                    tparam_names,
                                    t.clone(),
                                ));
                            }
                        }
                        // Skip any remaining tokens on this line.
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                        t.name.clone()
                    } else {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                        String::new()
                    };
                    if !alias.is_empty() && !target.is_empty() {
                        self.file.type_aliases.push((alias.clone(), target));
                        // The alias's declared visibility survives into `@Metadata`
                        // (`internal typealias` must stay module-bound for consumers).
                        let vis = visibility_of(&mods);
                        if vis != Visibility::Public {
                            self.file.type_alias_visibility.insert(alias, vis);
                        }
                    }
                }
                _ => {
                    self.diags
                        .error(self.tok().span, "expected a top-level declaration");
                    self.recover_to_decl_boundary();
                }
            }
            if is_expect {
                let after = self.file.decls.len();
                self.file
                    .expect_decls
                    .extend_from_slice(&self.file.decls[decls_before..after]);
            }
        }
    }

    /// Whether the current token can START a top-level declaration — a declaration keyword, an
    /// annotation `@`, or a soft-keyword/modifier identifier. Used by error recovery to resync.
    fn at_decl_start(&self) -> bool {
        use TokenKind::*;
        match self.kind() {
            KwFun | KwClass | KwVal | KwVar | KwImport | KwPackage | At => true,
            Ident => {
                is_modifier(self.text())
                    || matches!(
                        self.text(),
                        "object"
                            | "interface"
                            | "enum"
                            | "annotation"
                            | "typealias"
                            | "data"
                            | "companion"
                    )
            }
            _ => false,
        }
    }

    fn at_file_annotation(&self) -> bool {
        self.at(TokenKind::At)
            && self.t.get(self.i + 1).is_some_and(|token| {
                token.kind == TokenKind::Ident && token.text(self.src) == "file"
            })
            && self
                .t
                .get(self.i + 2)
                .is_some_and(|token| token.kind == TokenKind::Colon)
    }

    /// Return the token after a context parameter list, or `None` for a `context(...)` call.
    fn context_parameter_list_end_at(&self, open: usize) -> Option<usize> {
        if self.t.get(open)?.kind != TokenKind::LParen {
            return None;
        }
        let mut j = open + 1;
        loop {
            while self
                .t
                .get(j)
                .is_some_and(|token| token.kind == TokenKind::Newline)
            {
                if self.t[j].text(self.src) == ";" {
                    return None;
                }
                j += 1;
            }
            if self.t.get(j)?.kind == TokenKind::RParen {
                return Some(j + 1);
            }
            loop {
                let token = self.t.get(j)?;
                if token.kind == TokenKind::At {
                    j = self.annotation_end_at(j)?;
                    continue;
                }
                if token.kind == TokenKind::Ident
                    && is_modifier(token.text(self.src))
                    && self.t.get(j + 1).map(|next| next.kind) != Some(TokenKind::Colon)
                {
                    j += 1;
                    continue;
                }
                break;
            }
            if self.t.get(j)?.kind != TokenKind::Ident
                || self.t.get(j + 1)?.kind != TokenKind::Colon
            {
                return None;
            }
            j += 2;

            let mut parens = 0usize;
            let mut brackets = 0usize;
            let mut braces = 0usize;
            let mut angles = 0usize;
            let mut saw_type = false;
            loop {
                let token = self.t.get(j)?;
                match token.kind {
                    TokenKind::LParen => parens += 1,
                    TokenKind::RParen if parens > 0 => parens -= 1,
                    TokenKind::LBracket => brackets += 1,
                    TokenKind::RBracket if brackets > 0 => brackets -= 1,
                    TokenKind::LBrace => braces += 1,
                    TokenKind::RBrace if braces > 0 => braces -= 1,
                    TokenKind::Lt => angles += 1,
                    TokenKind::Gt if angles > 0 => angles -= 1,
                    TokenKind::Comma
                        if parens == 0 && brackets == 0 && braces == 0 && angles == 0 =>
                    {
                        if !saw_type {
                            return None;
                        }
                        j += 1;
                        break;
                    }
                    TokenKind::RParen if brackets == 0 && braces == 0 && angles == 0 => {
                        return saw_type.then_some(j + 1);
                    }
                    TokenKind::Eof => return None,
                    TokenKind::Newline if token.text(self.src) == ";" => return None,
                    _ => {}
                }
                saw_type |= !matches!(token.kind, TokenKind::Newline);
                j += 1;
            }
        }
    }

    /// Skip an unparseable top-level construct to the next declaration boundary, descending through
    /// balanced `{}` so a declaration keyword inside a body isn't mistaken for a boundary. Always makes
    /// progress (the current token is not a declaration start — that is why recovery was invoked).
    fn recover_to_decl_boundary(&mut self) {
        // Always consume the offending token first — it reached recovery precisely because no
        // declaration arm could parse it (it may even LOOK like a declaration start, e.g. `object`
        // with no name), so returning on it without progress would loop the caller forever.
        if !self.at(TokenKind::Eof) {
            self.bump();
        }
        let mut depth = 0i32;
        loop {
            match self.kind() {
                TokenKind::Eof => return,
                TokenKind::LBrace => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RBrace => {
                    depth -= 1;
                    self.bump();
                }
                _ if depth <= 0 && self.at_decl_start() => return,
                _ => {
                    self.bump();
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
            } else if self.at(TokenKind::Ident)
                && is_modifier(self.text())
                && self.t.get(self.i + 1).map(|t| t.kind) != Some(TokenKind::Colon)
            {
                // A modifier soft keyword immediately followed by `:` is a NAME, not a modifier
                // (`fun f(open: Int)`, `@Anno sealed: T`) — a real modifier is never followed by a colon.
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
    fn take_pending_annotations(&mut self) -> Vec<AnnotationRef> {
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

    /// Hoist nested interfaces and supported interface subclasses.
    fn register_interface_nested(&mut self, iface: &str, modifiers: &[String]) {
        let start = self.file.decls.len();
        let nested = self.parse_nested_type_decl();
        let implements = nested.supertypes.iter().any(|s| s.name == iface)
            || nested.base_class.as_deref() == Some(iface);
        if nested.is_interface() || implements {
            self.register_hoisted_nested_classifier(iface, start, nested, modifiers);
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

    /// Consume one declaration annotation and retain its full interned name. `None` for a use-site
    /// `@file:`/`@get:` target, which does not apply to the declaration itself.
    fn skip_annotation(&mut self) -> (Option<AnnotationRef>, Vec<ExprId>) {
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
        let annotation_span = self.tok().span;
        let qname = self.parse_qualified_name();
        if !qname.is_empty() {
            self.file
                .detached_type_refs
                .push(simple_type_ref(&qname, annotation_span));
            let suppressions = self
                .pending_annotations
                .iter()
                .zip(&self.pending_annotation_args)
                .filter(|(annotation, _)| {
                    annotation
                        .name
                        .rsplit('.')
                        .next()
                        .is_some_and(|name| name == "Suppress")
                })
                .flat_map(|(_, arguments)| arguments)
                .filter_map(|argument| self.file.const_string_value(*argument))
                .map(|value| value.to_lossy().to_owned())
                .collect::<Vec<_>>();
            if !suppressions.is_empty() {
                self.file
                    .detached_type_ref_suppressions
                    .insert(annotation_span.lo, suppressions);
            }
        }
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
                Some(AnnotationRef {
                    name: qname,
                    span: annotation_span,
                }),
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
            if self.at_named_arg() {
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
        // Annotation arrays, nested annotations, and spreads recurse while a declaration prefix is
        // still being consumed, before `parse_bp` or a class-like parser is necessarily entered.
        // They are expression syntax, so join the shared expression lane instead of owning another
        // origin-specific counter/recovery policy.
        self.with_nesting_guard(
            ParserNesting::Expression,
            |_parser, _span| None,
            Self::parse_annotation_value_inner,
        )
    }

    fn parse_annotation_value_inner(&mut self) -> Option<ExprId> {
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
        } else if self.at(TokenKind::Star) {
            // A spread argument in an annotation (`@A(*arrayOf("O"), "K")` — a `vararg` annotation
            // parameter). Annotation values are metadata krusty ignores, so just consume the `*` and
            // parse the spread expression to keep the argument list well-formed.
            self.bump(); // '*'
            self.parse_annotation_value()
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
        let annotations = self.take_pending_annotations();
        let _ = self.take_pending_annotation_args();
        let context_params = std::mem::take(&mut self.pending_context_params);
        let start = self.tok().span;
        let is_var = self.at(TokenKind::KwVar);
        self.bump(); // val/var
                     // Optional generic type parameters on an extension property (`val <T> T.foo: T`) —
                     // erased, but retained so they scope over the receiver, type, and accessor bodies.
        let (type_params, _tp_non_null, _tp_reified, type_param_bounds, _) =
            if self.at(TokenKind::Lt) {
                self.parse_type_params()
            } else {
                Default::default()
            };
        let (receiver, name) = self.parse_receiver_and_declaration_name("property name");
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let init_operator = self.eat_span(TokenKind::Eq);
        let mut init = if init_operator.is_some() {
            self.skip_newlines();
            Some(self.parse_expr())
        } else {
            None
        };
        if let (Some(operator), Some(init)) = (init_operator, init) {
            self.file.value_operator_spans.insert(init.0, operator);
        }
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
        let mut getter_ty: Option<TypeRef> = None;
        let mut setter: Option<PropAccessor> = None;
        let mut explicit_backing_field = None;
        // A bare `get`/`set` (default accessor with no body) was seen — the property then has a real
        // backing field and MUST be initialized (a bare accessor is not an abstract declaration).
        let mut saw_bare_accessor = false;
        'accessors: loop {
            let save = self.i;
            self.skip_newlines();
            let mut is_private = false;
            loop {
                self.skip_newlines();
                if self.at(TokenKind::At) {
                    let Some(end) = self.annotation_end_at(self.i) else {
                        self.i = save;
                        break 'accessors;
                    };
                    self.i = end;
                    continue;
                }
                if self.at(TokenKind::Ident)
                    && matches!(
                        self.text(),
                        "private" | "protected" | "internal" | "public" | "inline"
                    )
                {
                    is_private |= self.text() == "private";
                    self.bump();
                    continue;
                }
                break;
            }
            if self.explicit_backing_fields
                && !is_private
                && explicit_backing_field.is_none()
                && self.at(TokenKind::Ident)
                && self.text() == "field"
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| matches!(t.kind, TokenKind::Colon | TokenKind::Eq))
            {
                self.bump();
                let field_ty = if self.eat(TokenKind::Colon) {
                    Some(self.parse_type())
                } else {
                    None
                };
                let field_init_operator = self.eat_span(TokenKind::Eq);
                let field_init = if field_init_operator.is_some() {
                    self.skip_newlines();
                    Some(self.parse_expr())
                } else {
                    None
                };
                if init.is_some() && field_init.is_some() {
                    self.diags.error(
                        self.tok().span,
                        "krusty: a property with an explicit backing field cannot also have its own initializer",
                    );
                } else if let Some(field_init) = field_init {
                    if let Some(operator) = field_init_operator {
                        self.file
                            .value_operator_spans
                            .insert(field_init.0, operator);
                    }
                    init = Some(field_init);
                }
                explicit_backing_field = Some(ExplicitBackingField { ty: field_ty });
                continue;
            }
            if !self.at(TokenKind::Ident) || !matches!(self.text(), "get" | "set") {
                self.i = save; // not an accessor — restore (incl. any consumed newlines/modifier)
                break;
            }
            let is_get = self.text() == "get";
            self.bump(); // 'get' / 'set'
            if is_get {
                // A custom getter is `get() = expr` / `get() { … }`. A bare `get` or a `get()` with
                // no body is the (redundant) explicit DEFAULT getter — consume its optional `()` and
                // leave `getter` unset (the property keeps its default field accessor).
                let had_parens = self.eat_accessor_parens(false).is_some();
                getter_ty = self.eat(TokenKind::Colon).then(|| self.parse_type());
                if self.at(TokenKind::Eq) || self.at(TokenKind::LBrace) {
                    getter = Some(self.parse_accessor_body());
                } else if had_parens {
                    // `get()` with parens but no body is invalid (a bare `get` — no parens — is the
                    // default accessor).
                    self.diags.error(
                        self.tok().span,
                        "expected '=' or '{' for a property getter".to_string(),
                    );
                } else {
                    saw_bare_accessor = true;
                }
                let _ = is_private; // getter visibility not modeled (rare); ignored
            } else {
                // setter: optional `(param)` then optional body; `private set` has neither.
                let param = self.parse_setter_param();
                let body = if self.eat(TokenKind::Eq) {
                    self.skip_newlines();
                    Some(FunBody::Expr(self.parse_expr()))
                } else if self.at(TokenKind::LBrace) {
                    Some(FunBody::Block(self.parse_block_expr(false)))
                } else {
                    None // default-bodied setter (e.g. `private set`)
                };
                if body.is_none() {
                    saw_bare_accessor = true;
                }
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
            && context_params.is_empty()
        {
            self.diags.error(
                start,
                "krusty: a property without an initializer must be 'lateinit'",
            );
        }
        // A bare `get`/`set` (default accessor, no body) means the property has a real backing field,
        // so it is NOT an abstract declaration and MUST be initialized.
        if saw_bare_accessor
            && init.is_none()
            && delegate.is_none()
            && !is_lateinit
            && receiver.is_none()
            && context_params.is_empty()
        {
            self.diags.error(
                start,
                "krusty: a property with a default accessor must be initialized".to_string(),
            );
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        let getter_reads_field = getter
            .as_ref()
            .and_then(|g| match g {
                FunBody::Expr(e) | FunBody::Block(e) => Some(*e),
                FunBody::None => None,
            })
            .is_some_and(|e| self.expr_reads_field(e));
        PropDecl {
            is_open: false,
            name,
            annotations,
            context_params,
            decl_line: 0,
            visibility: Visibility::Public,
            type_params,
            type_param_bounds,
            receiver,
            ty,
            is_var,
            is_override: false,
            is_lateinit,
            getter,
            getter_ty,
            getter_reads_field,
            setter,
            is_const,
            is_abstract,
            delegate,
            explicit_backing_field,
            init,
            span: Span::new(start.lo, end.hi),
        }
    }

    /// Return the first token after an annotation without mutating parser state.
    fn annotation_end_at(&self, mut i: usize) -> Option<usize> {
        if !self
            .t
            .get(i)
            .is_some_and(|token| token.kind == TokenKind::At)
        {
            return None;
        }
        i += 1;
        if self
            .t
            .get(i)
            .is_some_and(|token| token.kind == TokenKind::Ident)
            && self
                .t
                .get(i + 1)
                .is_some_and(|token| token.kind == TokenKind::Colon)
        {
            i += 2;
        }
        if !self
            .t
            .get(i)
            .is_some_and(|token| token.kind == TokenKind::Ident)
        {
            return None;
        }
        i += 1;
        while self
            .t
            .get(i)
            .is_some_and(|token| token.kind == TokenKind::Dot)
            && self
                .t
                .get(i + 1)
                .is_some_and(|token| token.kind == TokenKind::Ident)
        {
            i += 2;
        }
        if !self
            .t
            .get(i)
            .is_some_and(|token| token.kind == TokenKind::LParen)
        {
            return Some(i);
        }
        let mut depth = 0usize;
        loop {
            match self.t.get(i).map(|token| token.kind) {
                Some(TokenKind::LParen) => depth += 1,
                Some(TokenKind::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                Some(TokenKind::Eof) | None => return None,
                _ => {}
            }
            i += 1;
        }
    }

    /// Detect an explicit primary-constructor prefix without consuming sibling annotations.
    fn primary_constructor_header_follows(&self) -> bool {
        let mut i = self.i;
        loop {
            i = self.after_plain_newlines(i);
            let Some(token) = self.t.get(i) else {
                return false;
            };
            if token.kind == TokenKind::At {
                let Some(end) = self.annotation_end_at(i) else {
                    return false;
                };
                i = end;
                continue;
            }
            if token.kind == TokenKind::Ident
                && is_modifier(token.text(self.src))
                && self
                    .t
                    .get(i + 1)
                    .is_none_or(|next| next.kind != TokenKind::Colon)
            {
                i += 1;
                continue;
            }
            return token.kind == TokenKind::Ident && token.text(self.src) == "constructor";
        }
    }

    /// Whether the expression tree at `e` reads the property backing field — a bare `field`
    /// identifier. Used to detect that a custom getter (`get() = field + …`) has a backing field.
    fn expr_reads_field(&self, e: ExprId) -> bool {
        if let Expr::Name(n) = self.file.expr(e) {
            if n == "field" {
                return true;
            }
        }
        self.file
            .any_child_expr(e, &mut |c| self.expr_reads_field(c), &mut |_| false)
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

    /// Parse a setter's optional `(param)` and return its parameter name.
    ///
    /// Kotlin gives setter parameters a deliberately narrower grammar than ordinary function
    /// parameters: annotations are allowed, while the property supplies the value type. Keep this
    /// parser narrow instead of routing through `parse_param_list`, which would require a type and
    /// admit unrelated modifiers/defaults. Annotation values are parsed through the common annotation
    /// grammar and then discarded because `PropAccessor` has no parameter-metadata contract.
    fn parse_setter_param(&mut self) -> Option<String> {
        if !self.eat(TokenKind::LParen) {
            return None;
        }
        self.skip_plain_newlines();
        while self.at(TokenKind::At) {
            let _ = self.skip_annotation();
            self.skip_plain_newlines();
        }
        let name = if self.at(TokenKind::Ident) {
            let n = self.text().to_string();
            self.bump();
            Some(n)
        } else {
            None
        };
        self.skip_plain_newlines();
        // Keep accepting the historical optional type annotation for recovery/compatibility, but use
        // the ordinary type parser so a complex type cannot desynchronize the enclosing class body.
        if self.eat(TokenKind::Colon) {
            self.skip_plain_newlines();
            let _ = self.parse_type();
            self.skip_plain_newlines();
        }
        // Kotlin permits a trailing comma in a multiline parameter list. There is exactly one setter
        // parameter, so consume only the comma; a second parameter remains visible to `expect` as an
        // error instead of being silently accepted.
        if self.eat(TokenKind::Comma) {
            self.skip_plain_newlines();
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
            FunBody::Block(self.parse_block_expr(false))
        } else {
            self.diags.error(
                self.tok().span,
                "expected '=' or '{' for a property getter".to_string(),
            );
            FunBody::None
        }
    }

    /// Parse `companion object [Name]` as an ordinary nested singleton class and return its arena id.
    /// The enclosing declaration keeps only this relationship edge; all declaration data belongs to
    /// the nested class itself.
    fn parse_companion(&mut self, outer: &str, modifiers: &[String]) -> DeclId {
        let nested_start = self.file.decls.len();
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let start = self.tok().span;
        self.bump(); // 'companion'
        self.bump(); // 'object'
        let simple_name = if self.at(TokenKind::Ident) {
            self.bump().text(self.src).to_string()
        } else {
            "Companion".to_string()
        };
        let name = format!("{outer}.{simple_name}");
        let (supertypes, base_class, base_type_args, base_args, delegations, delegation_exprs) =
            self.parse_supertypes();
        let mut methods = Vec::new();
        let mut props = Vec::new();
        let mut init_order = Vec::new();
        self.skip_newlines();
        if self.eat(TokenKind::LBrace) {
            loop {
                self.skip_newlines();
                let mods = self.parse_member_decl_prefix();
                let lateinit = mods.iter().any(|m| m == "lateinit");
                if self.parse_and_register_nested_classifier(&name, &mods, false) {
                    continue;
                }
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => {
                        let function = self.parse_fun(&mods);
                        methods.push(function);
                    }
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let mut property = self.parse_top_property_c(
                            lateinit,
                            false,
                            mods.iter().any(|m| m == "const"),
                            false,
                        );
                        property.visibility = visibility_of(&mods);
                        property.is_open = !mods.iter().any(|m| m == "final")
                            && mods.iter().any(|m| m == "open" || m == "override");
                        property.is_override = mods.iter().any(|m| m == "override");
                        init_order.push(ClassInit::PropInit(props.len()));
                        props.push(property);
                    }
                    TokenKind::Ident
                        if self.text() == "init"
                            && self
                                .t
                                .get(self.i + 1)
                                .is_some_and(|token| token.kind == TokenKind::LBrace) =>
                    {
                        self.bump();
                        init_order.push(ClassInit::Block(self.parse_block_expr(false)));
                    }
                    _ => {
                        self.diags.error(
                            self.tok().span,
                            "companion object body accepts class members",
                        );
                        self.bump();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "'}'");
        }
        let end = self.t[self.i.saturating_sub(1)].span;
        let declaration = ClassDecl {
            primary_ctor_visibility: Visibility::Public,
            name,
            visibility: visibility_of(modifiers),
            annotations,
            annotation_args,
            type_parameters: crate::ast::ClassTypeParameters::new(
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            props: Vec::new(),
            methods,
            companion: None,
            body_props: props,
            init_order,
            kind: ClassKind::Class,
            singleton: true,
            is_data: false,
            is_value: false,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations,
            delegation_exprs,
            base_class,
            base_type_args,
            base_args,
            secondary_ctors: Vec::new(),
            primary_ctor_annotations: Some(Vec::new()),
            span: Span::new(start.lo, end.hi),
            ctor_close_line: 0,
            decl_line: 0,
        };
        let id = self.file.add_decl(Decl::Class(declaration));
        self.file.decls.insert(nested_start, id);
        id
    }

    /// `enum class Name { A, B, C }` — v0: simple entries (no constructor args, no class body).
    fn parse_enum_inner(&mut self) -> ClassDecl {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let start = self.tok().span;
        self.bump(); // 'enum'
        self.bump(); // 'class'
        let name = self.ident_or_error("enum name");
        // Optional primary constructor: `enum class C(val rgb: Int, …)`.
        let mut props = Vec::new();
        let has_primary_ctor = self.eat(TokenKind::LParen);
        if has_primary_ctor {
            self.skip_newlines();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                let epmods = self.skip_decl_prefix();
                let is_vararg = epmods.iter().any(|m| m == "vararg");
                let is_property = self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar);
                let is_var = self.at(TokenKind::KwVar);
                if is_property {
                    self.bump();
                }
                let pspan = self.tok().span;
                let pname = self.ident_or_error("parameter name");
                self.expect(TokenKind::Colon, "':'");
                self.skip_newlines(); // a wrapped declaration puts the type on the next line (`val x:\n  T`)
                let ty = self.parse_type();
                // A default value (`enum class C(val x: Int = 1)`) — same as a regular class ctor param;
                // each enum entry that omits the argument gets it at its construction site.
                let default = if self.eat(TokenKind::Eq) {
                    self.skip_newlines();
                    Some(self.parse_expr())
                } else {
                    None
                };
                props.push(PropParam {
                    name: pname,
                    ty,
                    span: pspan,
                    decl_line: 0,
                    is_vararg,
                    is_var,
                    is_property,
                    is_override: epmods.iter().any(|modifier| modifier == "override"),
                    is_open: !epmods.iter().any(|modifier| modifier == "final")
                        && epmods
                            .iter()
                            .any(|modifier| modifier == "open" || modifier == "override"),
                    visibility: visibility_of(&epmods),
                    default,
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
        // Always enter the shared optional parser. Besides avoiding a declaration-specific colon
        // branch, this lets enum headers honor the same `NL* ':'` grammar as class, interface, object,
        // companion-object, and anonymous-object headers.
        let (enum_supertypes, _base, _base_targs, _args, _del, _del_e) = self.parse_supertypes();
        let mut entries: Vec<AstEnumEntry> = Vec::new();
        let mut methods = Vec::new();
        // Enum body member properties (`enum class C { A; val x = … }`) and their initializer order.
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        // A `companion object { … }` in the enum body (`enum class E { A; companion object { … } }`).
        let mut companion = None;
        let mut secondary_ctors: Vec<SecondaryCtor> = Vec::new();
        self.skip_newlines();
        if self.eat(TokenKind::LBrace) {
            // `enum class E {; ... }` has no entries. The lexer represents `;` and a physical line
            // break with the same token kind, but retains the source spelling; consume the separator
            // run once and remember whether it explicitly ended the entry list.
            let mut members_only = false;
            while self.at(TokenKind::Newline) {
                members_only |= self.text() == ";";
                self.bump();
            }
            if !members_only {
                loop {
                    // Enum constants may carry annotations (`@SerialName("system") SYSTEM(...)` — common
                    // in kotlinx.serialization enums). Capture them so they can be emitted onto the enum's
                    // static field (per JVM retention), matching kotlinc.
                    let mut entry_ann_names: Vec<AnnotationRef> = Vec::new();
                    let mut entry_ann_args: Vec<Vec<ExprId>> = Vec::new();
                    while self.at(TokenKind::At) {
                        let (name, args) = self.skip_annotation();
                        if let Some(n) = name {
                            entry_ann_names.push(n);
                            entry_ann_args.push(args);
                        }
                        self.skip_newlines();
                    }
                    if !self.at(TokenKind::Ident) {
                        break;
                    }
                    let entry_span = self.tok().span;
                    let entry_name = self.text().to_string();
                    self.bump();
                    // Optional constructor arguments: `RED(0xFF0000)`, incl. named `RED(rgb = 0xFF0000)`.
                    let mut args = Vec::new();
                    let mut arg_names: Vec<Option<String>> = Vec::new();
                    if self.eat(TokenKind::LParen) {
                        self.skip_newlines();
                        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                            // A named argument `name = value` — the identifier is followed by `=` (but not
                            // `==`). Capture the name so the lowering can reorder to constructor order.
                            let named = self.at_named_arg();
                            if named {
                                arg_names.push(Some(self.text().to_string()));
                                self.bump(); // name
                                self.bump(); // '='
                                self.skip_newlines();
                            } else {
                                arg_names.push(None);
                            }
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
                    let mut init_order = Vec::new();
                    if self.eat(TokenKind::LBrace) {
                        self.skip_newlines();
                        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                            let bmods = self.parse_member_decl_prefix();
                            let entry_owner = format!("{name}.{entry_name}");
                            let nested_start = self.file.decls.len();
                            if self.parse_and_register_nested_classifier(&entry_owner, &bmods, true)
                            {
                                // An `inner` classifier declared by an enum entry captures the entry value,
                                // whose source type is the enum—not a separately nameable entry-subclass
                                // type. Keep the JVM/source nesting path (`Enum.ENTRY.Inner`) while recording
                                // the semantic dispatch receiver (`Enum`) independently.
                                if bmods.iter().any(|modifier| modifier == "inner") {
                                    if let Some(&declaration) =
                                        self.file.decls[nested_start..].last()
                                    {
                                        if let Decl::Class(nested) = self.file.decl_mut(declaration)
                                        {
                                            nested.inner_of = Some(name.clone());
                                        }
                                    }
                                }
                            } else if self.at(TokenKind::KwFun) {
                                body.push(self.parse_fun(&bmods));
                            } else if self.at(TokenKind::KwVal) || self.at(TokenKind::KwVar) {
                                let property = self.parse_top_property(
                                    bmods.iter().any(|m| m == "lateinit"),
                                    false,
                                );
                                init_order.push(ClassInit::PropInit(bprops.len()));
                                bprops.push(property);
                            } else if self.at(TokenKind::Ident)
                                && self.text() == "init"
                                && self
                                    .t
                                    .get(self.i + 1)
                                    .is_some_and(|token| token.kind == TokenKind::LBrace)
                            {
                                self.bump(); // `init`
                                init_order.push(ClassInit::Block(self.parse_block_expr(false)));
                            } else {
                                self.diags.error(
                                    self.tok().span,
                                    "unsupported declaration in an enum entry body",
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
                        span: entry_span,
                        decl_line: 0,
                        annotations: entry_ann_names,
                        annotation_args: entry_ann_args,
                        args,
                        arg_names,
                        methods: body,
                        props: bprops,
                        init_order,
                    });
                    self.skip_newlines();
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
            }
            // Members follow a `;` separator (lexed as a newline): `enum class C { A, B; fun f() … }`.
            loop {
                self.skip_newlines();
                let emods = self.parse_member_decl_prefix();
                if self.parse_and_register_nested_classifier(&name, &emods, true) {
                    continue;
                }
                match self.kind() {
                    TokenKind::KwFun => {
                        let f = self.parse_fun(&emods);
                        methods.push(f);
                    }
                    // A body member property (`enum class C { A; val x = … }`): a field + accessor on
                    // the enum class, initialized in declaration order in the primary constructor.
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let mut p = self.parse_top_property_c(
                            emods.iter().any(|m| m == "lateinit"),
                            true,
                            emods.iter().any(|m| m == "const"),
                            false,
                        );
                        p.visibility = visibility_of(&emods);
                        p.is_open = !emods.iter().any(|x| x == "final")
                            && emods.iter().any(|x| x == "open" || x == "override");
                        p.is_override = emods.iter().any(|m| m == "override");
                        init_order.push(ClassInit::PropInit(body_props.len()));
                        body_props.push(p);
                    }
                    TokenKind::Ident
                        if self.text() == "init"
                            && self
                                .t
                                .get(self.i + 1)
                                .is_some_and(|token| token.kind == TokenKind::LBrace) =>
                    {
                        self.bump();
                        init_order.push(ClassInit::Block(self.parse_block_expr(false)));
                    }
                    TokenKind::Ident if self.text() == "constructor" => {
                        let annotations = self.take_pending_annotations();
                        let annotation_args = self.take_pending_annotation_args();
                        let ctor_span = self.tok().span;
                        self.bump(); // 'constructor'
                        let params = self.parse_param_list();
                        let mut delegation = CtorDelegation::None;
                        if self.eat(TokenKind::Colon) {
                            self.skip_newlines();
                            let target = if self.at(TokenKind::Ident) {
                                let target = self.text().to_string();
                                self.bump();
                                target
                            } else {
                                String::new()
                            };
                            let (args, names) = self.parse_call_arguments_with_names();
                            let call = CtorDelegationCall {
                                args,
                                names,
                                trailing_lambda: false,
                            };
                            delegation = match target.as_str() {
                                "this" => CtorDelegation::This(call),
                                "super" => CtorDelegation::Super(call),
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
                        let body = self
                            .at(TokenKind::LBrace)
                            .then(|| self.parse_block_expr(false));
                        secondary_ctors.push(SecondaryCtor {
                            annotations,
                            annotation_args,
                            params,
                            delegation,
                            body,
                            span: ctor_span,
                        });
                    }
                    TokenKind::Ident
                        if self.text() == "companion"
                            && self.t.get(self.i + 1).is_some_and(|t| {
                                t.kind == TokenKind::Ident && t.text(self.src) == "object"
                            }) =>
                    {
                        // `companion object { … }` in the enum body — parse it like a regular class's
                        // companion (anonymous name allowed) and attach its members to the enum.
                        companion = Some(self.parse_companion(&name, &emods));
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
            primary_ctor_visibility: Visibility::Public,
            name,
            visibility: Visibility::Public,
            annotations,
            annotation_args,
            type_parameters: crate::ast::ClassTypeParameters::new(
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            props,
            methods,
            companion,
            body_props,
            init_order,
            is_data: false,
            is_value: false,
            kind: ClassKind::Enum,
            singleton: false,
            enum_entries: entries,
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes: enum_supertypes,
            delegations: Vec::new(),
            delegation_exprs: Vec::new(),
            base_class: None,
            base_type_args: Vec::new(),
            base_args: Vec::new(),
            secondary_ctors,
            primary_ctor_annotations: has_primary_ctor.then_some(Vec::new()),
            span: Span::new(start.lo, end.hi),
            ctor_close_line: 0,
            decl_line: 0,
        }
    }

    /// Parse an optional generic constraint clause `where T : Bound, U : Bound2` after a function or
    /// class signature, returning the `(type parameter, bound)` pairs it declares.
    ///
    /// A `where` bound is the SAME constraint as the inline `<T : Bound>` form — Kotlin offers both
    /// spellings, and the second is required once a parameter has more than one bound. The pairs join
    /// the declaration's `type_param_bounds` so every consumer (erasure, member resolution on a value
    /// of the parameter's type) sees one list regardless of spelling; previously the clause was parsed
    /// for its diagnostics and then discarded, so `where T : Named` resolved no members at all while
    /// `<T : Named>` did.
    ///
    /// `where` may sit on a following line, so newlines are skipped only when the clause is actually
    /// present (otherwise the position is restored). Physical specialization of a bound belongs to
    /// emission; the parser retains every source-valid bound unchanged.
    fn parse_where_clause(
        &mut self,
        declared_type_params: &[String],
        declaration_name: &str,
    ) -> Vec<(String, TypeRef)> {
        let mut bounds: Vec<(String, TypeRef)> = Vec::new();
        let save = self.i;
        self.skip_newlines();
        if !(self.at(TokenKind::Ident) && self.text() == "where") {
            self.i = save;
            return bounds;
        }
        self.bump(); // 'where'
                     // Track per-name FUNCTION-TYPE bounds: an intersection (`where T : () -> Unit,
                     // T : (Boolean) -> Unit`) makes a `T` value convertible to several SAM shapes, and krusty's
                     // SAM conversion adapts lambda literals, not values behind an erased `T` — a call would
                     // pass the raw value where kotlinc synthesizes a wrapper (`ClassCastException`). Reject.
        let mut fn_bounds: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        loop {
            self.skip_newlines();
            let mut tp_name = String::new();
            let type_parameter_span = self.tok().span;
            if self.at(TokenKind::Ident) {
                tp_name = self.text().to_string();
                self.bump(); // type-parameter name
            }
            if !tp_name.is_empty() && !declared_type_params.iter().any(|name| name == &tp_name) {
                self.diags.error(
                    type_parameter_span,
                    format!(
                        "'{tp_name}' does not refer to a type parameter of '{declaration_name}'."
                    ),
                );
            }
            if self.eat(TokenKind::Colon) {
                let bound = self.parse_type();
                if !bound.fun_params.is_empty() || bound.name == "<fun>" {
                    let n = fn_bounds.entry(tp_name.clone()).or_default();
                    *n += 1;
                    if *n > 1 {
                        self.diags.error(
                            bound.span,
                            format!("krusty: type parameter '{tp_name}' with multiple function-type bounds is not supported"),
                        );
                    }
                }
                if !tp_name.is_empty() {
                    bounds.push((tp_name.clone(), bound));
                }
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        bounds
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

    fn parse_fun(&mut self, modifiers: &[String]) -> FunDecl {
        // Annotations consumed by `skip_decl_prefix` before this function, attached here (mirrors how
        // classes take them) so function-annotation plugins can see them; otherwise they are discarded.
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
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
            let (supertypes, _, _, _, _, _) = self.parse_supertypes();
            let _ = supertypes;
            if self.at(TokenKind::LBrace) {
                let _ = self.parse_block_expr(false);
            }
            return FunDecl {
                name: "<fun-interface>".to_string(),
                receiver: None,
                params: vec![],
                context_count: 0,
                ret: None,
                body: FunBody::None,
                type_params: vec![],
                type_param_bounds: Vec::new(),
                non_null_type_params: Default::default(),
                reified_type_params: Default::default(),
                span: start,
                signature_span: start,
                flags: FdFlags::default(),
                visibility: Visibility::Public,
                annotations,
                annotation_args,
                decl_line: 0, // filled by the parser post-pass
                body_close_line: 0,
            };
        }
        let (type_params, non_null_type_params, reified_type_params, type_param_bounds, _) =
            if self.at(TokenKind::Lt) {
                self.parse_type_params()
            } else {
                (
                    Vec::new(),
                    std::collections::HashSet::new(),
                    std::collections::HashSet::new(),
                    Vec::new(),
                    Vec::new(),
                )
            };
        let lexical_type_param_lens =
            self.push_lexical_type_params(&type_params, &type_param_bounds);
        let (receiver, name) = self.parse_receiver_and_declaration_name("extension function name");
        let mut params = self.parse_param_list();
        // Context parameters (`context(a: A) fun f()`), parsed at the declaration site into
        // `pending_context_params`, become LEADING value parameters (kotlinc's ABI) — prepend them and
        // record how many so the call-site resolver fills them implicitly.
        let context_count = self.pending_context_params.len();
        if context_count > 0 {
            let mut merged = std::mem::take(&mut self.pending_context_params);
            merged.append(&mut params);
            params = merged;
        }
        let ret = if self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        // A `where` bound is the same constraint as the inline `<T : Bound>` form; join them so every
        // consumer sees one list.
        let mut type_param_bounds = type_param_bounds;
        type_param_bounds.extend(self.parse_where_clause(&type_params, &name));
        let signature_end = self.t[self.i.saturating_sub(1)].span.hi;
        // A `=`-body or block body may sit on a following line (`fun f(): T\n{ … }`). Skip plain line
        // breaks to find it, restoring the position if what follows is neither — an abstract/no-body
        // function (no valid member/declaration begins with a bare `=` or `{`, so this is unambiguous).
        let body_save = self.i;
        self.skip_newlines();
        let body = if self.eat(TokenKind::Eq) {
            self.skip_newlines();
            FunBody::Expr(self.parse_expr())
        } else if self.at(TokenKind::LBrace) {
            FunBody::Block(self.parse_block_expr(false))
        } else {
            self.i = body_save;
            FunBody::None
        };
        let end = self.t[self.i.saturating_sub(1)].span;
        self.pop_lexical_type_params(lexical_type_param_lens);
        FunDecl {
            name,
            receiver,
            params,
            context_count,
            ret,
            body,
            type_params,
            type_param_bounds,
            non_null_type_params,
            reified_type_params,
            span: Span::new(start.lo, end.hi),
            signature_span: Span::new(start.lo, signature_end),
            flags: function_flags(modifiers),
            visibility: visibility_of(modifiers),
            annotations,
            annotation_args,
            decl_line: 0, // filled by the parser post-pass
            body_close_line: 0,
        }
    }

    fn parse_member_decl_prefix(&mut self) -> Vec<String> {
        self.pending_context_params.clear();
        let mut modifiers =
            if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                let modifiers = self.skip_decl_prefix();
                self.skip_newlines();
                modifiers
            } else {
                Vec::new()
            };
        modifiers.extend(self.maybe_parse_context_receivers());
        modifiers
    }

    fn local_declaration_after_prefix(&self) -> Option<TokenKind> {
        let mut i = self.i;
        let mut saw_context = false;
        loop {
            i = self.after_plain_newlines(i);
            let token = self.t.get(i)?;
            if token.kind == TokenKind::At {
                i = self.annotation_end_at(i)?;
                continue;
            }
            if token.kind == TokenKind::Ident
                && is_modifier(token.text(self.src))
                && self
                    .t
                    .get(i + 1)
                    .is_none_or(|next| next.kind != TokenKind::Colon)
            {
                i += 1;
                continue;
            }
            if !saw_context
                && token.kind == TokenKind::Ident
                && token.text(self.src) == "context"
                && self
                    .t
                    .get(i + 1)
                    .is_some_and(|next| next.kind == TokenKind::LParen)
            {
                i = self.context_parameter_list_end_at(i + 1)?;
                saw_context = true;
                continue;
            }
            return match token.kind {
                TokenKind::KwFun
                    if !self
                        .t
                        .get(i + 1)
                        .is_some_and(|next| next.kind == TokenKind::LParen) =>
                {
                    Some(TokenKind::KwFun)
                }
                TokenKind::KwVal | TokenKind::KwVar => Some(token.kind),
                _ => None,
            };
        }
    }

    /// Buffer a `context(...)` clause for the following function or property.
    fn maybe_parse_context_receivers(&mut self) -> Vec<String> {
        if !(self.at(TokenKind::Ident)
            && self.text() == "context"
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::LParen))
        {
            return Vec::new();
        }
        if !self.context_parameters {
            self.diags.error(
                self.tok().span,
                "the feature 'context parameters' is disabled".to_string(),
            );
        }
        self.bump(); // 'context'
        self.pending_context_params = self.parse_param_list();
        self.skip_newlines();
        // Modifiers/annotations may follow the context prefix (`context(a: A) private fun …`);
        // consume them so the declaration keyword is next, and RETURN them so the caller keeps the
        // visibility/modality (annotations buffer as pending, read by the declaration parser).
        if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
            let m = self.skip_decl_prefix();
            self.skip_newlines();
            m
        } else {
            Vec::new()
        }
    }

    fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.expect(TokenKind::LParen, "'('");
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let mut pmods = Vec::new();
            let mut pannos = Vec::new();
            let mut pannos_args = Vec::new();
            // `value` is a valid parameter name in Kotlin; only collect real parameter modifiers. A modifier
            // soft keyword used as a NAME (`fun f(open: Int)`) is left for the name parse below —
            // `skip_decl_prefix` stops before a modifier-ident that is immediately followed by `:`.
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

    /// Parse a value-argument list after its opening parenthesis.
    fn parse_call_argument_list(
        &mut self,
    ) -> (Vec<ExprId>, Vec<Option<String>>, Vec<Option<Span>>) {
        let mut args = Vec::new();
        let mut names: Vec<Option<String>> = Vec::new();
        let mut name_spans: Vec<Option<Span>> = Vec::new();
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            if self.at_named_arg() {
                let name_span = self.syntactic_ident_span(self.tok());
                let n = self.text().to_string();
                self.bump(); // name
                self.bump(); // '='
                self.skip_newlines();
                names.push(Some(n));
                name_spans.push(Some(name_span));
            } else {
                names.push(None);
                name_spans.push(None);
            }
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
        (args, names, name_spans)
    }

    fn parse_call_arguments_with_names(&mut self) -> (Vec<ExprId>, Vec<Option<String>>) {
        if !self.eat(TokenKind::LParen) {
            return (Vec::new(), Vec::new());
        }
        let (args, names, _) = self.parse_call_argument_list();
        self.expect(TokenKind::RParen, "')'");
        (args, names)
    }

    /// v0 class: `class Name(val/var p: Type, ...)` with an optional empty body `{}`.
    /// Every primary-constructor parameter must be a `val`/`var` property (no plain params yet).
    /// Extend the names of nested types hoisted DURING a child class's parse (decls `start..`) with the
    /// enclosing class's name, so `A { B { C } }` yields the FULL path `A.B.C` (internal `A$B$C`) instead
    /// of the truncated `B.C` the immediate-parent prefix alone produces. Applied at each level, this
    /// builds the complete path incrementally.
    fn reprefix_hoisted(&mut self, outer: &str, start: usize) {
        for k in start..self.file.decls.len() {
            let did = self.file.decls[k];
            if let crate::ast::Decl::Class(nc) = self.file.decl_mut(did) {
                let previous_root = nc.name.split('.').next().unwrap_or_default().to_string();
                nc.name = format!("{outer}.{}", nc.name);
                // `inner_of` is a semantic receiver identity, but at this point both it and the
                // descendant name are still relative to the child currently being published. Move
                // the receiver identity with the declaration path. Enum-entry inner classes replace
                // this field with their enum owner after registration, so their distinct semantic
                // receiver remains explicit rather than being inferred from the lexical path.
                if let Some(receiver) = nc.inner_of.as_deref() {
                    if receiver == previous_root
                        || receiver.starts_with(&format!("{previous_root}."))
                    {
                        nc.inner_of = Some(format!("{outer}.{receiver}"));
                    }
                }
            }
        }
    }

    /// Publish one already-parsed nested classifier and every descendant it hoisted while parsing.
    /// This is the single owner-identity boundary for class-like declarations: syntax-specific parsers
    /// may set class flags first, but visibility, lexical prefixing, and declaration-list registration
    /// must remain identical for class, object, interface, enum, and interface-member entry points.
    fn register_hoisted_nested_classifier(
        &mut self,
        outer: &str,
        start: usize,
        mut nested: ClassDecl,
        modifiers: &[String],
    ) {
        self.reprefix_hoisted(outer, start);
        nested.visibility = visibility_of(modifiers);
        nested.name = format!("{outer}.{}", nested.name);
        let id = self.file.add_decl(Decl::Class(nested));
        self.file.decls.insert(start, id);
    }

    /// Parse and register any class-like declaration that this parser can represent as a hoisted
    /// nested classifier. Class and object bodies deliberately share this funnel: the declaration's
    /// lexical owner, not the kind of its enclosing declaration, determines its source/JVM identity.
    ///
    /// `start` must precede the child parse because that parse may itself hoist descendants. Prefixing
    /// that complete suffix before registering the child preserves paths such as `A.B.C`; doing this in
    /// individual enum/class/object arms previously let otherwise-equivalent branches drift apart.
    fn parse_and_register_nested_classifier(
        &mut self,
        outer: &str,
        modifiers: &[String],
        enclosing_instance_exists: bool,
    ) -> bool {
        let is_inner = modifiers.iter().any(|modifier| modifier == "inner");
        let start = self.file.decls.len();
        let (mut nested, supports_inner) = match self.kind() {
            TokenKind::KwClass => {
                let nested = self.parse_class();
                (nested, true)
            }
            TokenKind::Ident
                if self.text() == "data"
                    && self.t.get(self.i + 1).is_some_and(|token| {
                        token.kind == TokenKind::KwClass
                            || (token.kind == TokenKind::Ident && token.text(self.src) == "object")
                    }) =>
            {
                self.bump(); // `data`
                let (mut nested, supports_inner) =
                    if self.at(TokenKind::Ident) && self.text() == "object" {
                        (self.parse_object(), false)
                    } else {
                        (self.parse_class(), true)
                    };
                nested.is_data = true;
                (nested, supports_inner)
            }
            TokenKind::Ident if self.text() == "interface" => (self.parse_interface(), false),
            TokenKind::Ident
                if self.text() == "enum"
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|token| token.kind == TokenKind::KwClass) =>
            {
                (self.parse_enum(), false)
            }
            TokenKind::Ident if self.text() == "object" => (self.parse_object(), false),
            _ => return false,
        };

        if is_inner && supports_inner && !enclosing_instance_exists {
            // An object is a singleton, so it cannot provide the per-instance receiver an `inner
            // class` requires. Consume the unsupported declaration without publishing a falsely
            // constructible classifier. The child parse may have tentatively hoisted descendants,
            // so remove that declaration-list suffix with the unsupported owner as one atomic unit.
            // Other classifier kinds ignore an inapplicable `inner` modifier rather than acquiring
            // class-only capture metadata.
            self.file.decls.truncate(start);
            return true;
        }

        if nested.kind == ClassKind::Class {
            nested.modality = modality_from_modifiers(modifiers);
        }
        if is_inner && supports_inner {
            nested.inner_of = Some(outer.to_string());
        }
        self.register_hoisted_nested_classifier(outer, start, nested, modifiers);
        true
    }

    /// Depth-guarded entries for the class-like declaration parsers. Nested type declarations
    /// (`class A { class B { … } }`, nested interfaces/objects/enums, and mixed chains) recurse
    /// through these without ever passing through `parse_bp` or `parse_stmt`, so they share their
    /// own bounded, stack-grown funnel (one counter with `parse_stmt`, bounding any interleaving).
    /// Past [`EXPR_DEPTH_LIMIT`] the declaration degrades to an error declaration with a
    /// diagnostic and a balanced skip — degrade, never crash.
    fn parse_class(&mut self) -> ClassDecl {
        self.guarded_class_like(Self::parse_class_inner)
    }

    fn parse_object(&mut self) -> ClassDecl {
        self.guarded_class_like(Self::parse_object_inner)
    }

    fn parse_interface(&mut self) -> ClassDecl {
        self.guarded_class_like(Self::parse_interface_inner)
    }

    fn parse_enum(&mut self) -> ClassDecl {
        self.guarded_class_like(Self::parse_enum_inner)
    }

    fn guarded_class_like(&mut self, inner: fn(&mut Self) -> ClassDecl) -> ClassDecl {
        self.with_nesting_guard(
            ParserNesting::Declaration,
            |parser, span| {
                // The rejected declaration's pending prefix (annotations, context params) dies
                // with it — otherwise it would attach to the next declaration after recovery.
                parser.pending_annotations.clear();
                parser.pending_annotation_args.clear();
                parser.pending_context_params.clear();
                error_class_decl(span)
            },
            inner,
        )
    }

    fn parse_class_inner(&mut self) -> ClassDecl {
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
        let (type_params, _, _, type_param_bounds, type_param_variances) = if self.at(TokenKind::Lt)
        {
            self.parse_type_params()
        } else {
            (
                Vec::new(),
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
                Vec::new(),
                Vec::new(),
            )
        };
        let lexical_type_param_lens =
            self.push_lexical_type_params(&type_params, &type_param_bounds);
        let mut primary_constructor_annotations = Vec::new();
        // An explicit constructor prefix may continue across physical newlines.
        let mut primary_ctor_visibility = Visibility::Public;
        if self.primary_constructor_header_follows() {
            self.skip_newlines();
            if self.at(TokenKind::At) || (self.at(TokenKind::Ident) && is_modifier(self.text())) {
                let ctor_mods = self.skip_decl_prefix();
                // The declared constructor visibility survives into `@Metadata` (and, for
                // `protected`, the JVM `<init>` access flags).
                primary_ctor_visibility = visibility_of(&ctor_mods);
                primary_constructor_annotations = self.take_pending_annotations();
                let _ = self.take_pending_annotation_args();
            }
        }
        let header_ctor_kw = self.at(TokenKind::Ident) && self.text() == "constructor";
        if header_ctor_kw {
            self.bump();
        }
        let mut props = Vec::new();
        let mut ctor_close_lo = 0u32;
        let has_primary_ctor_parens = self.eat(TokenKind::LParen);
        let header_has_primary = header_ctor_kw || has_primary_ctor_parens;
        if has_primary_ctor_parens {
            self.skip_newlines();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                let mut pannos = Vec::new();
                let mut pannos_args = Vec::new();
                let mut cpmods = Vec::new();
                if self.at(TokenKind::At)
                    || (self.at(TokenKind::Ident)
                        && is_modifier(self.text())
                        && self.text() != "value")
                {
                    cpmods = self.skip_decl_prefix(); // `private val x`, `@Anno val y`, `vararg xs`
                    pannos = self.take_pending_annotations();
                    pannos_args = self.take_pending_annotation_args();
                }
                let is_vararg = cpmods.iter().any(|m| m == "vararg");
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
                let pspan = self.tok().span;
                let pname = self.ident_or_error("parameter name");
                self.expect(TokenKind::Colon, "':'");
                self.skip_newlines(); // a wrapped declaration puts the type on the next line (`val x:\n  T`)
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
                    span: pspan,
                    decl_line: 0,
                    is_vararg,
                    is_var,
                    is_property,
                    is_override: cpmods.iter().any(|modifier| modifier == "override"),
                    is_open: !cpmods.iter().any(|modifier| modifier == "final")
                        && cpmods
                            .iter()
                            .any(|modifier| modifier == "open" || modifier == "override"),
                    visibility: visibility_of(&cpmods),
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
            // The byte offset of the primary ctor's `)` — rewritten to a source LINE by the
            // decl-line post-pass; kotlinc maps the ctor `$default`'s `return` to it.
            ctor_close_lo = self.tok().span.lo;
            self.expect(TokenKind::RParen, "')'");
        }
        // Optional supertype list: `: Iface1, Base(args), Iface2`. Supertypes with `()` are the
        // base class (v0: unsupported → flagged); the rest are implemented interfaces.
        let (supertypes, base_class, base_type_args, base_args, delegations, delegation_exprs) =
            self.parse_supertypes();
        // `class Derived<T> : Base<T>() where T : I1, T : I2` — generic constraints after the
        // supertype list, before the body. Same constraint as the inline form; one joined list.
        let mut type_param_bounds = type_param_bounds;
        type_param_bounds.extend(self.parse_where_clause(&type_params, &name));
        // Optional class body: member `fun`s, body properties (`val`/`var`), and `init { }` blocks.
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        let mut companion = None;
        let mut secondary_ctors: Vec<SecondaryCtor> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let mods = self.parse_member_decl_prefix();
                let lateinit = mods.iter().any(|m| m == "lateinit");
                let is_abstract = mods.iter().any(|m| m == "abstract");
                // All representable nested classifier kinds share one registration path. Keeping
                // this outside the member-kind match prevents class/object/enum syntax branches from
                // acquiring subtly different visibility or lexical-name behavior.
                if self.parse_and_register_nested_classifier(&name, &mods, true) {
                    continue;
                }
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => {
                        let f = self.parse_fun(&mods);
                        methods.push(f);
                    }
                    TokenKind::KwVal | TokenKind::KwVar => {
                        // Non-abstract body props may omit the initializer (init blocks supply the
                        // value); an `abstract` property has no field and is marked accordingly.
                        let mut p = self.parse_top_property_c(
                            lateinit,
                            !is_abstract,
                            mods.iter().any(|m| m == "const"),
                            is_abstract,
                        );
                        p.visibility = visibility_of(&mods);
                        p.is_open = !mods.iter().any(|x| x == "final")
                            && mods.iter().any(|x| x == "open" || x == "override");
                        p.is_override = mods.iter().any(|m| m == "override");
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
                        let block = self.parse_block_expr(false);
                        init_order.push(ClassInit::Block(block));
                    }
                    // A companion is a nested singleton declaration linked from this class.
                    TokenKind::Ident
                        if self.text() == "companion"
                            && self.t.get(self.i + 1).is_some_and(|t| {
                                t.kind == TokenKind::Ident && t.text(self.src) == "object"
                            }) =>
                    {
                        companion = Some(self.parse_companion(&name, &mods));
                    }
                    TokenKind::Ident
                        if self.text() == "annotation"
                            && self
                                .t
                                .get(self.i + 1)
                                .map_or(false, |t| t.kind == TokenKind::KwClass) =>
                    {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident if self.text() == "constructor" => {
                        let annotations = self.take_pending_annotations();
                        let annotation_args = self.take_pending_annotation_args();
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
                            let (args, names) = self.parse_call_arguments_with_names();
                            let delegation_call = crate::ast::CtorDelegationCall {
                                args,
                                names,
                                trailing_lambda: false,
                            };
                            delegation = match target.as_str() {
                                "this" => CtorDelegation::This(delegation_call),
                                "super" => CtorDelegation::Super(delegation_call),
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
                            Some(self.parse_block_expr(false))
                        } else {
                            None
                        };
                        secondary_ctors.push(SecondaryCtor {
                            annotations,
                            annotation_args,
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
            primary_ctor_visibility,
            name,
            visibility: Visibility::Public,
            annotations,
            annotation_args,
            type_parameters: crate::ast::ClassTypeParameters::new(
                type_params,
                type_param_bounds,
                type_param_variances,
            ),
            props,
            methods,
            companion,
            body_props,
            init_order,
            is_data: false,
            is_value: false,
            kind: ClassKind::Class,
            singleton: false,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations,
            delegation_exprs,
            base_class,
            base_type_args,
            base_args,
            // A class has a primary constructor when it wrote one (parens / `constructor` keyword) OR
            // declares no secondary constructors at all (then an implicit no-arg primary exists). Only a
            // class with secondary ctors and no header ctor has NO primary.
            primary_ctor_annotations: (header_has_primary || secondary_ctors.is_empty())
                .then_some(primary_constructor_annotations),
            secondary_ctors,
            span: Span::new(start.lo, end.hi),
            ctor_close_line: ctor_close_lo,
            decl_line: 0,
        }
    }

    /// Parse an optional `: Base(args), Iface1, Iface2` supertype list. A supertype with `()` is the
    /// base class (returns its name + ctor-arg expressions); the rest are implemented interfaces.
    #[allow(clippy::type_complexity)]
    fn parse_supertypes(
        &mut self,
    ) -> (
        Vec<TypeRef>,
        Option<String>,
        Vec<TypeRef>,
        Vec<ExprId>,
        Vec<(String, String, bool)>,
        Vec<(String, ExprId)>,
    ) {
        let mut ifaces: Vec<TypeRef> = Vec::new();
        let mut base: Option<String> = None;
        let mut base_type_args = Vec::new();
        let mut base_args = Vec::new();
        let mut delegations = Vec::new();
        let mut delegation_exprs = Vec::new();
        // Kotlin declaration headers permit physical line breaks before the supertype-list colon.
        // The shared lookahead deliberately stops at `;`, which terminates the declaration even though
        // the lexer represents it with the same token kind as a line break.
        self.skip_plain_newlines_before(TokenKind::Colon);
        if self.eat(TokenKind::Colon) {
            loop {
                self.skip_newlines();
                let sup_span = self.tok().span;
                // A FUNCTION-TYPE supertype (`class C : () -> R`, `(A) -> R`, `Recv.() -> R`): a class
                // implementing a function type implements `kotlin/jvm/functions/FunctionN` (arity N =
                // value parameters, with an extension receiver folded in as the first). Parse the type
                // and record the `FunctionN` interface. A `suspend` function-type supertype maps to a
                // different (`SuspendFunctionN`) shape krusty doesn't model — reject so the file skips.
                if self.at_function_type_supertype() {
                    let ft = self.parse_type();
                    let arity = ft.fun_params.len();
                    if ft.fun_suspend() || arity > 22 {
                        // A `suspend` function type (a distinct `SuspendFunctionN` shape) and an arity
                        // beyond the stdlib's `Function0..22` (big-arity `FunctionN`) are not modeled.
                        self.diags.error(
                            sup_span,
                            "krusty: this function-type supertype is not supported".to_string(),
                        );
                    } else {
                        let mut targs = ft.fun_params.clone();
                        if let Some(ret) = ft.arg.as_deref() {
                            targs.push(ret.clone());
                        }
                        ifaces.push(TypeRef {
                            name: crate::types::FUNCTION_N_INTERNAL[arity].to_string(),
                            // Keep the exact semantic function shape beside the physical FunctionN
                            // classifier. Receiver/context flags cannot be reconstructed from arity or
                            // the classifier name after parsing.
                            flags: ft
                                .flags
                                .with_nullable(false)
                                .with_definitely_non_null(false),
                            arg: ft.arg.clone(),
                            targs,
                            span: sup_span,
                            fun_params: ft.fun_params.clone(),
                            fun_context_count: ft.fun_context_count,
                        });
                    }
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
                let name = self.parse_qualified_name();
                let simple = name.rsplit('.').next().unwrap_or(&name).to_string();
                // Fully-qualified name (e.g. java.util.RandomAccess) → JVM internal format.
                let effective = if name.contains('.') {
                    name.replace('.', "/")
                } else {
                    simple.clone()
                };
                // Type arguments (`A<Int, Number>`) are erased in JVM DESCRIPTORS, but kotlinc records them
                // in the class `Signature` attribute (`LOperation<Lkotlin/Result<..>;>;`) so a reader
                // recovers a member's concrete generic return — kept on the interface `TypeRef`. Also note
                // whether any is a non-nullable primitive (`A<Long>` delegation needs substituted bridges).
                let targs: Vec<TypeRef> = if self.at(TokenKind::Lt) {
                    self.parse_type_args()
                } else {
                    Vec::new()
                };
                let has_primitive_targ = targs.iter().any(|ta| {
                    !ta.nullable()
                        && matches!(
                            ta.name.as_str(),
                            "Int"
                                | "Long"
                                | "Short"
                                | "Byte"
                                | "Char"
                                | "Boolean"
                                | "Double"
                                | "Float"
                        )
                });
                if self.eat(TokenKind::LParen) {
                    // constructor call → base class. Arguments may be NAMED (`Base(name = …, addr = …)`);
                    // the per-arg name is recorded so the checker/lowerer reorder to the base ctor order.
                    self.skip_newlines();
                    let mut args = Vec::new();
                    let mut arg_names: Vec<Option<String>> = Vec::new();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        let named = self.at_named_arg();
                        if named {
                            arg_names.push(Some(self.text().to_string()));
                            self.bump(); // name
                            self.bump(); // '='
                            self.skip_newlines();
                        } else {
                            arg_names.push(None);
                        }
                        // Spread `*expr` — an array spread into the base ctor's `vararg` parameter.
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
                    self.expect(TokenKind::RParen, "')'");
                    let mut reference = simple_type_ref(&name, sup_span);
                    reference.targs = targs.clone();
                    self.file.detached_type_refs.push(reference);
                    base = Some(effective.clone());
                    base_type_args = targs;
                    if arg_names.iter().any(|n| n.is_some()) {
                        if let Some(first) = args.first() {
                            self.file.base_arg_names.insert(first.0, arg_names);
                        }
                    }
                    base_args = args;
                } else if !effective.is_empty() {
                    ifaces.push(TypeRef {
                        name: effective.clone(),
                        flags: TrFlags::default()
                            .with_nullable(false)
                            .with_definitely_non_null(false),
                        arg: None,
                        targs,
                        span: sup_span,
                        fun_params: Vec::new(),
                        fun_context_count: 0,
                    });
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
                            delegations.push((effective.clone(), delegate, has_primitive_targ));
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
        (
            ifaces,
            base,
            base_type_args,
            base_args,
            delegations,
            delegation_exprs,
        )
    }

    /// `interface Name { fun sig(): T }` — abstract member functions only (v0).
    fn parse_interface_inner(&mut self) -> ClassDecl {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let start = self.tok().span;
        self.bump(); // 'interface'
        let name = self.ident_or_error("interface name");
        let (type_params, _, _, type_param_bounds, type_param_variances) = if self.at(TokenKind::Lt)
        {
            self.parse_type_params()
        } else {
            (
                Vec::new(),
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
                Vec::new(),
                Vec::new(),
            )
        };
        let (supertypes, _base, _base_type_args, _base_args, _, _) = self.parse_supertypes();
        // `interface I<T> where T : Bound` — generic constraints after the supertype list, before the
        // body. Same constraint as the inline form; one joined list.
        let mut type_param_bounds = type_param_bounds;
        type_param_bounds.extend(self.parse_where_clause(&type_params, &name));
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut companion = None;
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let imods = self.parse_member_decl_prefix();
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => {
                        let f = self.parse_fun(&imods);
                        methods.push(f);
                    }
                    // Abstract interface property: `val`/`var x: T` (no initializer/getter).
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let mut p = self.parse_top_property(false, true);
                        p.visibility = visibility_of(&imods);
                        p.is_override = imods.iter().any(|m| m == "override");
                        if p.init.is_some() {
                            self.diags.error(p.span, "krusty: interface properties with an initializer/getter are not supported");
                        }
                        body_props.push(p);
                    }
                    // A sealed interface's nested SUBCLASSES (`sealed interface I { data object O : I;
                    // data class C(…) : I }`) — hoist each to a top-level `I.O`/`I.C` (internal `I$O`/`I$C`),
                    // like a nested type in a class body. Only a nested type that IMPLEMENTS the enclosing
                    // interface is hoisted: a plain nested helper (`interface B { class Z { … b.priv() } }`)
                    // may call a private interface member through a synthetic accessor krusty doesn't
                    // synthesize, so those are still dropped (the file skips) rather than miscompiled.
                    TokenKind::KwClass => {
                        self.register_interface_nested(&name, &imods);
                    }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(
                                self.text(),
                                "data" | "enum" | "annotation" | "value"
                            ) && self.t.get(self.i + 1).map_or(false, |t| {
                                // `data class` / `enum class` / … and also `data object` (Kotlin 1.9).
                                t.kind == TokenKind::KwClass
                                    || (t.kind == TokenKind::Ident && t.text(self.src) == "object")
                            })) =>
                    {
                        self.register_interface_nested(&name, &imods);
                    }
                    TokenKind::Ident if self.text() == "typealias" => {
                        while !self.at(TokenKind::Newline) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                    }
                    // `interface I { companion object { … } }` — same as a class companion.
                    TokenKind::Ident
                        if self.text() == "companion"
                            && self.t.get(self.i + 1).is_some_and(|t| {
                                t.kind == TokenKind::Ident && t.text(self.src) == "object"
                            }) =>
                    {
                        companion = Some(self.parse_companion(&name, &imods));
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
            primary_ctor_visibility: Visibility::Public,
            name,
            visibility: Visibility::Public,
            annotations,
            annotation_args,
            type_parameters: crate::ast::ClassTypeParameters::new(
                type_params,
                type_param_bounds,
                type_param_variances,
            ),
            props: Vec::new(),
            methods,
            companion,
            body_props,
            init_order: Vec::new(),
            is_data: false,
            is_value: false,
            kind: ClassKind::Interface,
            singleton: false,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations: Vec::new(),
            delegation_exprs: Vec::new(),
            base_class: None,
            base_type_args: Vec::new(),
            base_args: Vec::new(),
            secondary_ctors: Vec::new(),
            primary_ctor_annotations: Some(Vec::new()),
            span: Span::new(start.lo, end.hi),
            ctor_close_line: 0,
            decl_line: 0,
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
                let mods = self.parse_member_decl_prefix();
                let lateinit = mods.iter().any(|m| m == "lateinit");
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => {
                        let f = self.parse_fun(&mods);
                        methods.push(f);
                    }
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let mut p = self.parse_top_property_c(
                            lateinit,
                            true,
                            mods.iter().any(|m| m == "const"),
                            false,
                        );
                        p.visibility = visibility_of(&mods);
                        p.is_open = !mods.iter().any(|x| x == "final")
                            && mods.iter().any(|x| x == "open" || x == "override");
                        p.is_override = mods.iter().any(|m| m == "override");
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
                        let block = self.parse_block_expr(false);
                        init_order.push(ClassInit::Block(block));
                    }
                    TokenKind::KwClass => {
                        let _ = self.parse_nested_type_decl();
                    }
                    TokenKind::Ident
                        if matches!(self.text(), "object" | "interface")
                            || (matches!(
                                self.text(),
                                "data" | "enum" | "annotation" | "value"
                            ) && self.t.get(self.i + 1).map_or(false, |t| {
                                // `data class` / `enum class` / … and also `data object` (Kotlin 1.9).
                                t.kind == TokenKind::KwClass
                                    || (t.kind == TokenKind::Ident && t.text(self.src) == "object")
                            })) =>
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

    fn parse_anon_object(&mut self, span: Span) -> ExprId {
        self.bump(); // 'object'
        let (supertypes, base_class, base_type_args, base_args, delegations, delegation_exprs) =
            self.parse_supertypes();
        let (methods, body_props, init_order) = self.parse_object_body();
        let end = self.t[self.i.saturating_sub(1)].span;
        let name = format!("Anon$anon${}", span.lo);
        let synth = ClassDecl {
            primary_ctor_visibility: Visibility::Public,
            name: name.clone(),
            visibility: Visibility::Public,
            annotations: Vec::new(),
            annotation_args: Vec::new(),
            type_parameters: crate::ast::ClassTypeParameters::invariant(
                self.current_lexical_type_params(),
                self.current_lexical_type_param_bounds(),
            ),
            props: Vec::new(),
            methods,
            companion: None,
            body_props,
            init_order,
            is_data: false,
            is_value: false,
            kind: ClassKind::Class,
            singleton: false,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations,
            delegation_exprs,
            base_class,
            base_type_args,
            base_args,
            secondary_ctors: Vec::new(),
            primary_ctor_annotations: Some(Vec::new()),
            span: Span::new(span.lo, end.hi),
            ctor_close_line: 0,
            decl_line: 0,
        };
        let did = self.file.add_decl(Decl::Class(synth));
        self.file.decls.push(did);
        let callee = self.file.add_expr(Expr::Name(name), span);
        let construction = self.file.add_expr(
            Expr::Call {
                callee,
                args: Vec::new(),
            },
            Span::new(span.lo, end.hi),
        );
        self.file.anonymous_object_classes.insert(construction, did);
        construction
    }

    fn parse_object_inner(&mut self) -> ClassDecl {
        let annotations = self.take_pending_annotations();
        let annotation_args = self.take_pending_annotation_args();
        let start = self.tok().span;
        self.bump(); // 'object'
        let name = self.ident_or_error("object name");
        // Capture the object's implemented INTERFACES (`object X : KSerializer<C>`) AND a base class
        // (`object A : Sealed()`): the general class lowering/emit handles the `extends` + `super(args)`.
        let (supertypes, base_class, base_type_args, base_args, delegations, delegation_exprs) =
            self.parse_supertypes();
        let mut methods = Vec::new();
        let mut body_props: Vec<PropDecl> = Vec::new();
        let mut init_order: Vec<ClassInit> = Vec::new();
        self.skip_newlines();
        if self.at(TokenKind::LBrace) {
            self.bump();
            loop {
                self.skip_newlines();
                let mods = self.parse_member_decl_prefix();
                let lateinit = mods.iter().any(|m| m == "lateinit");
                // Named singleton owners use the same classifier-registration invariant as class
                // owners. The flag only excludes `inner` classes, whose semantics require an
                // enclosing instance that a singleton cannot supply. Anonymous-object bodies use
                // `parse_object_body` instead and have no stable owner identity to publish here.
                if self.parse_and_register_nested_classifier(&name, &mods, false) {
                    continue;
                }
                match self.kind() {
                    TokenKind::RBrace | TokenKind::Eof => break,
                    TokenKind::KwFun => {
                        let f = self.parse_fun(&mods);
                        methods.push(f);
                    }
                    TokenKind::KwVal | TokenKind::KwVar => {
                        let mut p = self.parse_top_property_c(
                            lateinit,
                            true,
                            mods.iter().any(|m| m == "const"),
                            false,
                        ); // init blocks may supply the value
                        p.visibility = visibility_of(&mods);
                        p.is_open = !mods.iter().any(|x| x == "final")
                            && mods.iter().any(|x| x == "open" || x == "override");
                        p.is_override = mods.iter().any(|m| m == "override");
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
                        let block = self.parse_block_expr(false);
                        init_order.push(ClassInit::Block(block));
                    }
                    TokenKind::Ident
                        if self.text() == "annotation"
                            && self
                                .t
                                .get(self.i + 1)
                                .map_or(false, |t| t.kind == TokenKind::KwClass) =>
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
            primary_ctor_visibility: Visibility::Public,
            name,
            visibility: Visibility::Public,
            annotations,
            annotation_args,
            type_parameters: crate::ast::ClassTypeParameters::new(
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            props: Vec::new(),
            methods,
            companion: None,
            body_props,
            init_order,
            is_data: false,
            is_value: false,
            kind: ClassKind::Class,
            singleton: true,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality: crate::ast::Modality::Final,
            inner_of: None,
            supertypes,
            delegations,
            delegation_exprs,
            base_class,
            base_type_args,
            base_args,
            secondary_ctors: Vec::new(),
            primary_ctor_annotations: Some(Vec::new()),
            span: Span::new(start.lo, end.hi),
            ctor_close_line: 0,
            decl_line: 0,
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

    /// Depth-guarded entry for the type parser. Every type recursion funnels through here —
    /// nested parens (`((((Int))))`), generic arguments (`parse_type_args`), function-type
    /// parameters/returns, and `Array<…>` elements all re-enter via `parse_type` — so this single
    /// guard bounds all of it. Type recursion runs OUTSIDE `parse_bp` in declaration position, so
    /// the expression guard never sees it. Past [`EXPR_DEPTH_LIMIT`] the type degrades to an error
    /// type with a diagnostic and an angle-aware balanced skip — degrade, never crash.
    fn parse_type(&mut self) -> TypeRef {
        self.with_nesting_guard(
            ParserNesting::Type,
            |_parser, span| simple_type_ref("<error>", span),
            Self::parse_type_inner,
        )
    }

    /// Parse a type, folding a trailing definitely-non-null intersection `T & Any` (the only legal
    /// intersection in Kotlin source) into the left operand with `nullable = false`. `T & Any` erases
    /// identically to `T`; its only observable effect is that a value of it is non-null, which the
    /// `as` cast enforces at runtime (a null assertion). The checker verifies that the left side denotes
    /// a type parameter; the parser validates and consumes the required bare `Any` right side.
    fn parse_type_inner(&mut self) -> TypeRef {
        let mut ty = self.parse_type_atom();
        while self.at(TokenKind::Amp) {
            self.bump(); // '&'
            let any = self.parse_type_atom();
            let valid_lhs = !ty.nullable()
                && !ty.definitely_non_null()
                && ty.arg.is_none()
                && ty.targs.is_empty()
                && ty.fun_params.is_empty();
            let valid_rhs = any.name == "Any"
                && !any.nullable()
                && !any.definitely_non_null()
                && any.arg.is_none()
                && any.targs.is_empty()
                && any.fun_params.is_empty();
            if !valid_lhs || !valid_rhs {
                self.diags.error(
                    if valid_lhs { any.span } else { ty.span },
                    "a definitely non-null type must have the form 'T & Any'",
                );
            }
            ty.set_nullable(false);
            ty.set_definitely_non_null(true);
        }
        ty
    }

    fn parse_type_atom(&mut self) -> TypeRef {
        // Leading type annotations (`@Composable () -> Unit`, `@UnsafeVariance T`): consume them and
        // record by the type's start offset so a plugin can recover them via `TypeRef.span.lo`.
        // Without this, an `@` before a type would fail to parse. NOTE: a following `(` is NOT consumed
        // as an annotation argument list here — in type position it belongs to a function type
        // (`@Composable () -> Unit`); an argument-bearing type annotation (`@Foo(1) Bar`, rare) is not
        // yet handled.
        let mut type_anns = Vec::new();
        while self.at(TokenKind::At) {
            self.bump(); // '@'
            let annotation_span = self.tok().span;
            let qname = self.parse_qualified_name();
            if !qname.is_empty() {
                self.file
                    .detached_type_refs
                    .push(simple_type_ref(&qname, annotation_span));
            }
            self.parse_type_args(); // `@Foo<Bar>` — type arguments on the annotation
                                    // An argument-bearing type annotation `@Ann("a") String`. A following `(` is annotation
                                    // args ONLY if it is NOT the parameter list of a function type — `@Composable () -> Unit`
                                    // has the `(` belong to the type. Disambiguate by peeking past the balanced `(…)`: an
                                    // `->` after it means a function type (leave the `(`), otherwise consume the args.
            if self.at(TokenKind::LParen) && !self.paren_group_precedes_arrow(self.i) {
                let _ = self.parse_annotation_args();
            }
            if !qname.is_empty() {
                type_anns.push(AnnotationRef {
                    name: qname,
                    span: annotation_span,
                });
            }
            // The grammar is `annotation NL*` — a line break may separate the annotation from the type
            // it annotates (and from a following annotation), as in a wrapped type-argument list:
            //     List<
            //         @Ann(with = S::class)
            //         Outer.Inner,
            //     >
            // Skip those breaks so the type is parsed next; `span` below then still points at the TYPE
            // token, which is the key `type_annotations` is recorded under. A `;` is NOT a line break
            // here (it cannot separate an annotation from its type), so leave it to fail as before.
            while self.at(TokenKind::Newline) && !self.at_semicolon() {
                self.bump();
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
        // Context-receiver function type (`+ContextParameters`): `context(A, B) (params) -> R`. A context
        // receiver is modeled as a LEADING function-type parameter — identical to `(A, B, params) -> R`,
        // matching how context parameters lower (and so a plain function type converts to a context one).
        // Only recognized before a `(`-started function type; `context` stays a valid ordinary type name.
        let mut context_types: Vec<TypeRef> = Vec::new();
        if self.at(TokenKind::Ident)
            && self.text() == "context"
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::LParen)
        {
            self.bump(); // 'context'
            self.bump(); // '('
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                // Optional `name: Type` — a named context receiver.
                if self.at(TokenKind::Ident)
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::Colon)
                {
                    self.bump(); // name
                    self.bump(); // ':'
                }
                context_types.push(self.parse_type());
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "')'");
            // A `(`-started plain function type absorbs the context receivers as leading params below. An
            // EXTENSION function type (`context(O) K.(A) -> R`) folds them in ahead of its receiver — the
            // `Ident`-started branch prepends `context_types` to `[receiver, params…]`. A context receiver
            // on a NON-function type is invalid; the guard at that branch's tail rejects it (never a
            // silently mis-arity function type).
        }
        // Function type: `(A, B) -> R` — starts with `(`.
        if self.at(TokenKind::LParen) {
            self.bump(); // '('
            self.skip_newlines();
            let fun_context_count = context_types.len() as u32;
            let mut fun_params = std::mem::take(&mut context_types);
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
                self.skip_newlines();
            }
            self.skip_newlines();
            self.expect(TokenKind::RParen, "')'");
            if self.eat(TokenKind::Arrow) {
                let ret = self.parse_type();
                let nullable = self.eat_type_nullable();
                TypeRef {
                    name: "<fun>".to_string(),
                    flags: TrFlags::default()
                        .with_nullable(nullable)
                        .with_definitely_non_null(false)
                        .with_fun_suspend(fun_suspend),
                    arg: Some(Box::new(ret)),
                    targs: Vec::new(),
                    span,
                    fun_params,
                    fun_context_count,
                }
            } else if fun_params.len() == 1 && !fun_suspend {
                // A PARENTHESIZED type used for grouping (no `->` follows the `)`), most commonly to make
                // a function type nullable: `(() -> Unit)?` ≡ `Function0<Unit>?`. The parens wrap a single
                // type; an optional trailing `?` applies to it. (Kotlin permits redundant parens around any
                // type — `(Int)`, `(String)?`.)
                let mut inner = fun_params.into_iter().next().unwrap();
                if self.eat_type_nullable() {
                    inner.set_nullable(true);
                }
                inner
            } else {
                // Parenthesized multi-element type (a tuple) — krusty doesn't support tuple types.
                self.diags.error(span, "expected '->' for function type");
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
            // Every classifier uses the same type-argument structure. Projection semantics belong to
            // resolution; the parser retains exactly what was written.
            let mut targs = self.parse_type_args();
            let arg = None;
            // A generic-qualified nested type `Outer<A>.Inner<B>.Innermost<C>`: the dotted-path loop
            // above stops at the `<` after `Outer`, so continue consuming `.Nested` segments (each with
            // its own erased type arguments) here. The dotted name (`Outer.Inner.Innermost`) resolves
            // the nested class; the intermediate segments' arguments are erased in JVM descriptors, so
            // only the last segment's arguments are retained (matching the plain `Outer.Inner` path).
            while self.at(TokenKind::Dot)
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| t.kind == TokenKind::Ident)
            {
                self.bump(); // '.'
                name.push('.');
                name.push_str(self.text());
                self.bump(); // nested type name
                if self.at(TokenKind::Lt) {
                    targs = self.parse_type_args();
                }
            }
            let nullable = self.eat_type_nullable(); // `T?`
            let base = TypeRef {
                name,
                flags: TrFlags::default()
                    .with_nullable(nullable)
                    .with_definitely_non_null(false),
                arg,
                targs,
                span,
                fun_params: Vec::new(),
                fun_context_count: 0,
            };
            // Receiver (extension) function type `Recv.() -> R` ≡ `Function1<Recv, R>`, and
            // `Recv.(A) -> R` ≡ `Function2<Recv, A, R>`. The receiver folds in as the first function
            // parameter, exactly how Kotlin lowers an extension-function type to `FunctionN` — so the
            // rest of the pipeline sees a plain `(Recv, …) -> R`. (The dotted-path loop above stops at
            // `.` `(` since `(` is not an `Ident`, leaving us positioned here.)
            if !self.parsing_anonymous_function_receiver
                && self.at(TokenKind::Dot)
                && self
                    .t
                    .get(self.i + 1)
                    .map_or(false, |t| t.kind == TokenKind::LParen)
            {
                self.bump(); // '.'
                self.bump(); // '('
                let fun_context_count = context_types.len() as u32;
                let mut fun_params = std::mem::take(&mut context_types);
                fun_params.push(base);
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
                    flags: TrFlags::default()
                        .with_nullable(fnull)
                        .with_definitely_non_null(false)
                        .with_fun_has_receiver(true)
                        .with_fun_suspend(fun_suspend),
                    arg: Some(Box::new(ret)),
                    targs: Vec::new(),
                    span,
                    fun_params,
                    fun_context_count,
                };
            }
            // A `context(…)` receiver must precede a FUNCTION type; on a plain type it is invalid — reject
            // rather than silently dropping the context receivers (which would mis-type the value).
            if !context_types.is_empty() {
                self.diags.error(
                    span,
                    "krusty: a context receiver is only valid on a function type".to_string(),
                );
                return TypeRef {
                    name: "<error>".to_string(),
                    flags: TrFlags::default()
                        .with_nullable(false)
                        .with_definitely_non_null(false),
                    arg: None,
                    targs: Vec::new(),
                    span,
                    fun_params: Vec::new(),
                    fun_context_count: 0,
                };
            }
            base
        } else {
            self.diags.error(span, "expected a type");
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

    fn skip_variance(&mut self) -> (bool, bool) {
        if self.at(TokenKind::KwIn) {
            self.bump();
            return (true, false);
        }
        if self.at(TokenKind::Ident) && self.text() == "out" {
            self.bump();
            return (false, true);
        }
        (false, false)
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
            let (in_projection, out_projection) = self.skip_variance();
            if self.eat(TokenKind::Star) {
                // Star projection `<*>` — erased to `Any?`.
                let span = self.tok().span;
                args.push(TypeRef {
                    name: "Any".to_string(),
                    flags: TrFlags::default()
                        .with_nullable(true)
                        .with_definitely_non_null(false)
                        // `Any?` is the correct semantic bound, while this marker retains the one
                        // source distinction needed by erased runtime checks. Without it,
                        // `Function1<*, *>` and `Function1<Any?, Any?>` become indistinguishable:
                        // Kotlin permits the former in `is` and rejects the latter as erased.
                        .with_star_projection(true),
                    arg: None,
                    targs: Vec::new(),
                    span,
                    fun_params: Vec::new(),
                    fun_context_count: 0,
                });
            } else {
                let mut argument = self.parse_type();
                argument.set_projection(in_projection, out_projection);
                args.push(argument);
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

    fn parse_receiver_and_declaration_name(
        &mut self,
        name_context: &'static str,
    ) -> (Option<TypeRef>, String) {
        if self.at(TokenKind::LParen) {
            let receiver_start = self.tok().span.lo;
            let mut receiver = self.parse_type();
            let receiver_hi = self.t[self.i.saturating_sub(1)].span.hi;
            receiver.span = Span::new(receiver_start, receiver_hi);
            self.expect(TokenKind::Dot, "'.'");
            return (Some(receiver), self.ident_or_error(name_context));
        }

        let receiver_start = self.tok().span.lo;
        let first = self.ident_or_error(name_context);
        if !self.at(TokenKind::Dot) && !self.at(TokenKind::Lt) && !self.at(TokenKind::Question) {
            return (None, first);
        }

        let mut receiver_targs = if self.at(TokenKind::Lt) {
            self.parse_type_args()
        } else {
            Vec::new()
        };
        let mut nullable = self.eat_type_nullable();
        let mut receiver_hi = self.t[self.i.saturating_sub(1)].span.hi;
        self.expect(TokenKind::Dot, "'.'");
        let mut receiver_name = first;

        let declaration_name = loop {
            let segment = self.ident_or_error(name_context);
            if segment == "<error>" {
                break segment;
            }
            let segment_hi = self.t[self.i.saturating_sub(1)].span.hi;
            if self.at(TokenKind::Lt) || self.at(TokenKind::Question) {
                let segment_targs = if self.at(TokenKind::Lt) {
                    self.parse_type_args()
                } else {
                    Vec::new()
                };
                nullable = self.eat_type_nullable();
                receiver_name.push('.');
                receiver_name.push_str(&segment);
                receiver_targs = segment_targs;
                receiver_hi = self.t[self.i.saturating_sub(1)].span.hi;
                self.expect(TokenKind::Dot, "'.'");
            } else if self.eat(TokenKind::Dot) {
                receiver_name.push('.');
                receiver_name.push_str(&segment);
                receiver_targs.clear();
                nullable = false;
                receiver_hi = segment_hi;
            } else {
                break segment;
            }
        };
        (
            Some(TypeRef {
                name: receiver_name,
                flags: TrFlags::default()
                    .with_nullable(nullable)
                    .with_definitely_non_null(false),
                arg: None,
                targs: receiver_targs,
                span: Span::new(receiver_start, receiver_hi),
                fun_params: Vec::new(),
                fun_context_count: 0,
            }),
            declaration_name,
        )
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
        Vec<crate::types::TypeVariance>,
    ) {
        let mut names = Vec::new();
        let mut non_null = std::collections::HashSet::new();
        let mut reified = std::collections::HashSet::new();
        let mut bounds: Vec<(String, TypeRef)> = Vec::new();
        let mut variances = Vec::new();
        if !self.eat(TokenKind::Lt) {
            return (names, non_null, reified, bounds, variances);
        }
        loop {
            self.skip_newlines();
            // Skip variance/reified modifiers. `in` is a keyword; `out`/`reified` are idents.
            let mut is_reified = false;
            let mut variance = crate::types::TypeVariance::Invariant;
            while (self.at(TokenKind::Ident) && matches!(self.text(), "reified" | "out"))
                || self.at(TokenKind::KwIn)
            {
                if self.at(TokenKind::Ident) && self.text() == "reified" {
                    is_reified = true;
                } else if self.at(TokenKind::Ident) && self.text() == "out" {
                    variance = crate::types::TypeVariance::Out;
                } else if self.at(TokenKind::KwIn) {
                    variance = crate::types::TypeVariance::In;
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
                variances.push(variance);
                if is_reified {
                    reified.insert(tname.clone());
                }
            }
            if self.eat(TokenKind::Colon) {
                let bound = self.parse_type();
                // `T: Any` → the type param can't be null (erased to Object but non-null).
                if bound.name == "Any" && !bound.nullable() && !tname.is_empty() {
                    non_null.insert(tname.clone());
                }
                // Retain the semantic bound. JVM specialization/boxing is not syntax validity.
                // Retain explicit bounds independently of erasure.
                if !tname.is_empty() {
                    bounds.push((tname.clone(), bound));
                }
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "'>'");
        (names, non_null, reified, bounds, variances)
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
        let has_params = self.lambda_arrow_before_close(self.i);
        // Parameter type annotations, parallel to `params` — kept (in a side-table) so a bare-value
        // lambda `{ x: Int -> … }` types its own parameters even without an expected function type.
        let mut param_types: Vec<Option<TypeRef>> = Vec::new();
        // A destructured lambda parameter `{ (a, b) -> … }` binds ONE (synthetic) parameter, then
        // `val (a, b) = <synthetic>` is prepended to the body — reusing the `Stmt::Destructure`
        // machinery. Collected here, spliced after the body statements are parsed.
        // (synthetic param name, destructured entries `(name, is_var)`, span) per `(a, b)` param.
        type LambdaDestructure = (String, Vec<(String, bool)>, Vec<Option<String>>, Span);
        let mut destructures: Vec<LambdaDestructure> = Vec::new();
        let params = if has_params {
            let mut ps = Vec::new();
            loop {
                self.skip_newlines();
                if self.at(TokenKind::LParen) {
                    let sp = self.tok().span;
                    self.bump();
                    let mut entries = Vec::new();
                    let mut source_props: Vec<Option<String>> = Vec::new();
                    loop {
                        // Full form (`{ (val a, val b) -> … }`): each component carries its own
                        // `val`/`var` and binds by property name. Keyword-less short form is positional
                        // unless the short-form flag is on.
                        let is_var = self.at(TokenKind::KwVar);
                        let had_kw = is_var || self.at(TokenKind::KwVal);
                        if had_kw {
                            self.bump();
                        }
                        let n = self.ident_or_error("variable name");
                        // A per-entry type annotation (`(a: Int, b) ->`) is tolerated, ignored.
                        if self.eat(TokenKind::Colon) {
                            let _ = self.parse_type();
                        }
                        // By-name entry (`(a = prop) ->`) or short-form (`(a, b) ->` binds by own name).
                        let source = if self.name_based_destructuring && self.eat(TokenKind::Eq) {
                            let src = self.ident_or_error("property name");
                            if self.eat(TokenKind::Colon) {
                                let _ = self.parse_type();
                            }
                            Some(src)
                        } else if self.short_form_destructuring || had_kw {
                            Some(n.clone())
                        } else {
                            None
                        };
                        entries.push((n, is_var));
                        source_props.push(source);
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
                    destructures.push((synth, entries, source_props, sp));
                } else if self.name_based_destructuring && self.at(TokenKind::LBracket) {
                    // The short-form bracket destructuring `{ [a, b] -> … }` (NameBasedDestructuring) —
                    // identical to the `(a, b)` form, just with `[ ]`.
                    let sp = self.tok().span;
                    self.bump();
                    let mut entries = Vec::new();
                    loop {
                        // Full-form bracket (`{ [val a, val b] -> … }`) — component keyword optional,
                        // positional either way.
                        let is_var = self.at(TokenKind::KwVar);
                        if is_var || self.at(TokenKind::KwVal) {
                            self.bump();
                        }
                        let n = self.ident_or_error("variable name");
                        if self.eat(TokenKind::Colon) {
                            let _ = self.parse_type();
                        }
                        entries.push((n, is_var));
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
                    // The `[a, b]` bracket form is positional (`componentN`), never by-name.
                    let source_props = vec![None; entries.len()];
                    destructures.push((synth, entries, source_props, sp));
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
        let saved_block_value = self.block_trailing_is_value;
        self.block_trailing_is_value = true;
        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            stmts.push(self.parse_stmt());
        }
        self.block_trailing_is_value = saved_block_value;
        // Prepend `val (a, b) = <synthetic-param>` for each destructured parameter (reversed so the
        // first parameter's binding ends up first).
        for (synth, entries, source_props, sp) in destructures.into_iter().rev() {
            let init = self.file.add_expr(Expr::Name(synth), sp);
            let d = self.file.add_stmt(Stmt::Destructure { entries, init }, sp);
            if source_props.iter().any(|s| s.is_some()) {
                self.file.destructure_source_props.insert(d.0, source_props);
            }
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

    /// Anonymous function expression: `fun (params): T = expr` / `fun (params): T { … }`. Desugars to a
    /// lambda (`Expr::Lambda`) carrying each parameter's declared type in the `lambda_param_types`
    /// side-table, so the value types even without an expected function type. An expression body
    /// (`= expr`) becomes a `Block` whose only value is that expression; a block body reuses the normal
    /// statement parser, so a `return` inside returns from the anonymous function (it lowers to the
    /// lambda's own `invoke`). A receiver form `fun R.(…)` records `R` separately and uses the same
    /// semantic path as any other receiver lambda.
    fn parse_anon_fun(&mut self, context_params: Vec<Param>) -> ExprId {
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
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            // `_` marks an unused parameter; keep the name so the arity is preserved.
            let name = self.ident_or_error("parameter name");
            let ty = if self.eat(TokenKind::Colon) {
                Some(self.parse_type())
            } else {
                None
            };
            params.push(name);
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

    fn parse_block_expr(&mut self, trailing_is_value: bool) -> ExprId {
        let start = self.tok().span;
        self.expect(TokenKind::LBrace, "'{'");
        let saved_block_value = self.block_trailing_is_value;
        self.block_trailing_is_value = trailing_is_value;
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            stmts.push(self.parse_stmt());
        }
        self.block_trailing_is_value = saved_block_value;
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
            _ if ty.nullable() => Expr::NullLit,
            "Int" | "Byte" | "Short" => Expr::IntLit(0),
            "Long" => Expr::LongLit(0),
            "Float" => Expr::FloatLit(0.0),
            "Double" => Expr::DoubleLit(0.0),
            "Boolean" => Expr::BoolLit(false),
            "Char" => Expr::CharLit(0),
            _ => Expr::NullLit,
        };
        self.file.add_expr(e, span)
    }

    /// Desugar `name++`/`name--`/`++name`/`--name` (statement) to `name = name ± 1`.
    fn parse_incdec(
        &mut self,
        name: String,
        dec: bool,
        prefix: bool,
        start: Span,
        target: Span,
    ) -> StmtId {
        self.finish_assignment_stmt(Stmt::IncDec { name, dec, prefix }, start, target)
    }

    /// Preserve the value of a member/index inc/dec at the end of ANY value-capable block. Both
    /// forms evaluate the getter/index read exactly once: postfix saves the old value before
    /// assigning `old.inc()`, while prefix saves `target.inc()` before assigning and returning that
    /// same new value. Keeping the result in a parser-reserved local also avoids the tempting but
    /// incorrect prefix expansion `target = target.inc(); target`, which invokes a custom getter a
    /// second time. Receiver/index expressions still have to be pure because the assignment side of
    /// the current AST reuses them; unsupported targets produce a diagnostic rather than being
    /// double-evaluated.
    fn incdec_access_value(
        &mut self,
        e: ExprId,
        target: ExprId,
        dec: bool,
        prefix: bool,
        start: Span,
    ) -> StmtId {
        let target_span = self.assignment_target_span(target);
        if !self.incdec_access_is_pure(target) {
            self.diags.error(
                self.file.expr_spans[e.0 as usize],
                "krusty: '++'/'--' is only supported on a simple variable or pure access path",
            );
            return self.finish_stmt(Stmt::Expr(e), start);
        }
        let op_name = if dec { "dec" } else { "inc" };
        let target_read = self
            .file
            .add_expr(self.file.expr(target).clone(), target_span);
        let saved_value = if prefix {
            self.build_inc_dec_call(target_read, op_name, target_span)
        } else {
            target_read
        };
        // Keep the saved value in the parser's synthetic-name namespace. Even an escaped source
        // binding with the same spelling cannot interfere: this generated block contains only its
        // own local and generated reads, and repeated expansions have independent scopes.
        const SAVED: &str = "$$incDecValue";
        let local = self.file.add_stmt(
            Stmt::Local {
                is_var: false,
                name: SAVED.to_string(),
                ty: None,
                init: saved_value,
            },
            start,
        );
        let saved_read = self
            .file
            .add_expr(Expr::Name(SAVED.to_string()), target_span);
        let assigned_value = if prefix {
            saved_read
        } else {
            self.build_inc_dec_call(saved_read, op_name, target_span)
        };
        let assignment = self
            .finish_incdec_access_assignment(target, assigned_value, start, target_span)
            .expect("pure member/index target has an assignment form");
        let trailing = self
            .file
            .add_expr(Expr::Name(SAVED.to_string()), target_span);
        let block = self.file.add_expr(
            Expr::Block {
                stmts: vec![local, assignment],
                trailing: Some(trailing),
            },
            Span::new(start.lo, self.file.expr_spans[e.0 as usize].hi),
        );
        self.finish_stmt(Stmt::Expr(block), start)
    }

    /// A full-form destructuring statement starts with `(` (name-based) or `[` (positional, only
    /// under `+NameBasedDestructuring`) IMMEDIATELY followed by a `val`/`var` keyword — the marker
    /// that distinguishes it from a parenthesized-expression statement.
    fn at_full_form_destructure(&self) -> bool {
        // Full-form destructuring is part of the `NameBasedDestructuring` feature — without it,
        // `(val a, …)` at statement position stays a (rejected) expression, matching kotlinc.
        if !self.name_based_destructuring {
            return false;
        }
        let opener = self.at(TokenKind::LParen) || self.at(TokenKind::LBracket);
        opener
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| matches!(t.kind, TokenKind::KwVal | TokenKind::KwVar))
    }

    /// Parse a full-form destructuring declaration: `(val a, val b) = e` / `[var a, var b] = e`,
    /// where each component carries its own `val`/`var` (and optional `: T` / `= sourceProp`). The
    /// paren form binds each component BY PROPERTY NAME (name-based); the bracket form is positional
    /// (`componentN`). Reuses `Stmt::Destructure` + the `destructure_source_props` side-table.
    fn parse_full_form_destructure(&mut self, start: Span) -> StmtId {
        let close = if self.at(TokenKind::LParen) {
            TokenKind::RParen
        } else {
            TokenKind::RBracket
        };
        self.bump(); // '(' or '['
        let mut entries: Vec<(String, bool)> = Vec::new();
        let mut source_props: Vec<Option<String>> = Vec::new();
        loop {
            // Each component declares its own mutability.
            let is_var = self.at(TokenKind::KwVar);
            if is_var || self.at(TokenKind::KwVal) {
                self.bump();
            } else {
                self.diags
                    .error(self.tok().span, "expected 'val' or 'var'".to_string());
            }
            let name = self.ident_or_error("variable name");
            if self.eat(TokenKind::Colon) {
                let _ = self.parse_type();
            }
            // `val newName = sourceProp` — bind `newName` from the receiver's `sourceProp` property.
            // A plain paren component `val a` binds by its OWN name; a bracket component is positional.
            let source = if self.eat(TokenKind::Eq) {
                let src = self.ident_or_error("property name");
                if self.eat(TokenKind::Colon) {
                    let _ = self.parse_type();
                }
                Some(src)
            } else if close == TokenKind::RParen {
                Some(name.clone())
            } else {
                None
            };
            entries.push((name, is_var));
            source_props.push(source);
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
        let stmt = self.finish_stmt(Stmt::Destructure { entries, init }, start);
        if source_props.iter().any(|s| s.is_some()) {
            self.file
                .destructure_source_props
                .insert(stmt.0, source_props);
        }
        stmt
    }

    /// Depth-guarded entry for the statement parser. Nested statement structures — block bodies
    /// (`while (…) { while (…) { … } }`), local declarations — recurse `parse_stmt` →
    /// `parse_block_expr` → `parse_stmt` without ever passing through `parse_bp`, so they need
    /// their own bounded, stack-grown funnel. Past [`EXPR_DEPTH_LIMIT`] the statement degrades to
    /// an error statement with a diagnostic and a balanced skip — degrade, never crash — leaving
    /// one closer for each enclosing block frame.
    fn parse_stmt(&mut self) -> StmtId {
        self.with_nesting_guard(
            ParserNesting::Statement,
            |parser, span| {
                let expression = parser
                    .file
                    .add_expr(Expr::Name("<error>".to_string()), span);
                parser.finish_stmt(Stmt::Expr(expression), span)
            },
            Self::parse_stmt_inner,
        )
    }

    fn parse_stmt_inner(&mut self) -> StmtId {
        // Labeled loop: `l1@ while(…)` / `l1@ for(…)` / `l1@ do {…}`. Capture the label and thread it
        // onto the loop so `break@l1`/`continue@l1` can target it.
        let mut loop_label: Option<String> = None;
        if self.at(TokenKind::Ident) {
            let next1 = self.t.get(self.i + 1);
            let mut loop_token = self.i + 2;
            while self
                .t
                .get(loop_token)
                .is_some_and(|token| token.kind == TokenKind::Newline)
            {
                loop_token += 1;
            }
            let is_label = next1.map_or(false, |t| t.kind == TokenKind::At)
                && self.t.get(loop_token).is_some_and(|t| {
                    matches!(
                        t.kind,
                        TokenKind::KwWhile | TokenKind::KwFor | TokenKind::KwDo
                    )
                });
            if is_label {
                loop_label = Some(self.text().to_string());
                self.bump(); // label name
                self.bump(); // '@'
                self.skip_newlines();
            }
        }
        // A modifier-prefixed LOCAL function (`tailrec fun f(…)`, `suspend fun g(…)`) is a
        // declaration statement in any body, not just scripts — without the prefix scan the
        // leading soft keyword parses as an expression name and reports itself unresolved.
        let modifier_prefixed_local_fun = !self.is_script
            && matches!(self.kind(), TokenKind::Ident | TokenKind::At)
            && self.local_declaration_after_prefix() == Some(TokenKind::KwFun);
        if self.is_script || modifier_prefixed_local_fun {
            if let Some(kind) = self.local_declaration_after_prefix() {
                let start = self.tok().span;
                if kind == TokenKind::KwFun {
                    let mods = self.parse_member_decl_prefix();
                    let function = self.parse_fun(&mods);
                    return self.finish_stmt(Stmt::LocalFun(function), start);
                }
                if self.kind() != kind {
                    let mods = self.parse_member_decl_prefix();
                    if !self.pending_context_params.is_empty() {
                        self.diags.error(
                            start,
                            "context receivers on local properties are not supported",
                        );
                        self.pending_context_params.clear();
                    }
                    self.pending_annotations.clear();
                    self.pending_annotation_args.clear();
                    if kind == TokenKind::KwVar
                        && mods.iter().any(|modifier| modifier == "lateinit")
                    {
                        self.bump();
                        let name = self.ident_or_error("variable name");
                        self.expect(TokenKind::Colon, "':'");
                        let ty = self.parse_type();
                        return self.finish_stmt(Stmt::LocalLateinit { name, ty }, start);
                    }
                    return self.parse_stmt();
                }
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
        if self.at(TokenKind::Ident)
            && self.text() == "lateinit"
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| token.kind == TokenKind::KwVar)
        {
            let start = self.tok().span;
            self.bump();
            self.bump();
            let name = self.ident_or_error("variable name");
            self.expect(TokenKind::Colon, "':'");
            let ty = self.parse_type();
            return self.finish_stmt(Stmt::LocalLateinit { name, ty }, start);
        }
        if self.at(TokenKind::Ident)
            && self.text() == "context"
            && self.local_declaration_after_prefix() == Some(TokenKind::KwFun)
        {
            let start = self.tok().span;
            let mods = self.parse_member_decl_prefix();
            let function = self.parse_fun(&mods);
            return self.finish_stmt(Stmt::LocalFun(function), start);
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
                    // NAME-BASED destructuring (`+NameBasedDestructuring`): an entry `newName = sourceProp`
                    // binds `newName` to the receiver's `sourceProp` property (not `componentN`). Parallel
                    // to `entries`; `None` for a positional entry.
                    let mut source_props: Vec<Option<String>> = Vec::new();
                    loop {
                        let n = self.ident_or_error("variable name");
                        // A per-entry type annotation (`val (a: Int, b) = …`) is tolerated, ignored.
                        if self.eat(TokenKind::Colon) {
                            let _ = self.parse_type();
                        }
                        // `newName = sourceProp` — the by-name renaming form. This `=` is inside the
                        // destructuring parens/brackets, distinct from the initializer `=` after `)`.
                        // Under `+EnableNameBasedDestructuringShortForm`, a plain PAREN entry `(a, b)`
                        // binds each variable to the receiver property of the SAME name.
                        let source = if self.name_based_destructuring && self.eat(TokenKind::Eq) {
                            let src = self.ident_or_error("property name");
                            if self.eat(TokenKind::Colon) {
                                let _ = self.parse_type();
                            }
                            Some(src)
                        } else if self.short_form_destructuring && close == TokenKind::RParen {
                            Some(n.clone())
                        } else {
                            None
                        };
                        entries.push((n, is_var));
                        source_props.push(source);
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
                    let stmt = self.finish_stmt(Stmt::Destructure { entries, init }, start);
                    if source_props.iter().any(|s| s.is_some()) {
                        self.file
                            .destructure_source_props
                            .insert(stmt.0, source_props);
                    }
                    return stmt;
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
                // `val`/`var x: T` with no initializer (deferred assignment) → synthesize the type's
                // default value (`0`/`false`/`null`); a later `x = …` assigns it. Kotlin's definite-
                // assignment guarantees the synthetic default is always overwritten before a read, so a
                // deferred `val` behaves like a once-assigned `var` — treat it as internally mutable
                // (krusty doesn't enforce assign-once; kotlinc already rejects misuse). A NULLABLE `val`
                // is left out: assigning a non-null value to it relies on smart-cast-after-assignment
                // that the checker doesn't yet model, so keep rejecting it (skip, never miscompile).
                let deferred = ty.is_some()
                    && !self.at(TokenKind::Eq)
                    && (is_var || !ty.as_ref().unwrap().nullable());
                let init_operator = (!deferred).then(|| {
                    let operator = self.tok().span;
                    self.expect(TokenKind::Eq, "'='");
                    operator
                });
                let init = if deferred {
                    let sp = self.tok().span;
                    self.default_init_expr(ty.as_ref().unwrap(), sp)
                } else {
                    self.skip_newlines();
                    self.parse_expr()
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
                let body = self.parse_loop_body();
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
                // An EMPTY do-while body (`do while (cond);`): the `do` is immediately followed by the
                // `while` keyword, so there is no body statement. (A non-do loop body may itself be a
                // `while` loop, so this empty-body shortcut is do-while-specific.)
                let body_span = self.tok().span;
                self.skip_newlines();
                let body = if self.at(TokenKind::KwWhile) {
                    self.file.add_expr(
                        Expr::Block {
                            stmts: Vec::new(),
                            trailing: None,
                        },
                        body_span,
                    )
                } else {
                    self.parse_loop_body()
                };
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
            TokenKind::KwFun
                if !self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|token| token.kind == TokenKind::LParen) =>
            {
                let function = self.parse_fun(&[]);
                self.finish_stmt(Stmt::LocalFun(function), start)
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
                // A nested type inside this local class is hoisted to `file.decls` DURING the parse
                // below, named by the path from the local class (`Local.Inner`). `hoist_local_classes`
                // later qualifies the local class itself, and these have to move with it — so record
                // exactly which declarations belong to it rather than guessing from a name prefix.
                let nested_start = self.file.decls.len();
                let mut d = self.parse_nested_type_decl();
                // Preserves the prior behavior: this path applied open/abstract but left `is_sealed`
                // at its default `false` (so a local `sealed` class never reported `is_sealed`).
                d.modality = modality_of(is_open, is_abstract, false);
                let nested: Vec<crate::ast::DeclId> = self.file.decls[nested_start..].to_vec();
                let stmt = self.finish_stmt(Stmt::LocalClass(d), start);
                if !nested.is_empty() {
                    self.file.local_class_nested.insert(stmt, nested);
                }
                stmt
            }
            // Full-form destructuring (`+NameBasedDestructuring`): `(val a, val b) = e` /
            // `[val a, val b] = e`, where each component carries its OWN `val`/`var` (unlike the
            // leading-keyword short form `val (a, b) = e`). Paren form is name-based (binds by
            // property name); bracket form is positional (`componentN`). Disambiguated from a plain
            // parenthesized-expression statement by the `val`/`var` right after the opener.
            _ if self.at_full_form_destructure() => self.parse_full_form_destructure(start),
            _ => {
                let e = self.parse_expr();
                // Increment/decrement *statement* (`target++` / `++target`): `parse_prefix`/
                // `parse_postfix` built an `Expr::IncDec`; in statement position the value is
                // discarded, so re-route to the statement helper (which desugars a `Name` to
                // `Stmt::IncDec` and a member/index target to an assignment). Immediately before a
                // closing brace of a VALUE-CONSUMING block, keep the expression value. The enclosing
                // parser context marks lambda/if/when/try value blocks; function/accessor/init/
                // constructor/loop/finally blocks stay on this statement path. This avoids both a
                // lambda-only fork and accidentally treating a Unit function's trailing implicit
                // property increment as a local-variable expression.
                if let Expr::IncDec {
                    target,
                    dec,
                    prefix,
                } = self.file.expr(e).clone()
                {
                    // Peek WITHOUT consuming: only a `}` (after newlines) makes this the body's last
                    // statement — consuming the newlines here would shift the statement's spans.
                    let after_nl = (self.i..self.t.len())
                        .find(|&i| self.t[i].kind != TokenKind::Newline)
                        .map(|i| self.t[i].kind);
                    if after_nl != Some(TokenKind::RBrace) || !self.block_trailing_is_value {
                        let op_span = self.file.expr_spans[e.0 as usize];
                        return self.incdec_target(target, dec, prefix, op_span, start);
                    }
                    // A block's trailing statement can be its VALUE. A `Name` target lowers directly
                    // as an expression; member/index targets use the shared, single-read expansion.
                    if !matches!(self.file.expr(target), Expr::Name(_)) {
                        return self.incdec_access_value(e, target, dec, prefix, start);
                    }
                }
                // assignment: `name = value` or `receiver.name = value`.
                if self.at(TokenKind::Eq) {
                    let target_span = self.assignment_target_span(e);
                    match self.file.expr(e).clone() {
                        Expr::Name(n) => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            return self.finish_assignment_stmt(
                                Stmt::Assign { name: n, value },
                                start,
                                target_span,
                            );
                        }
                        Expr::Member { receiver, name } => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            return self.finish_assignment_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                    safe: false,
                                },
                                start,
                                target_span,
                            );
                        }
                        Expr::Index { array, indices } => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            return self.finish_assignment_stmt(
                                Stmt::AssignIndex {
                                    array,
                                    indices,
                                    value,
                                },
                                start,
                                target_span,
                            );
                        }
                        Expr::SafeCall {
                            receiver,
                            name,
                            args: None,
                        } => {
                            let operator = self.bump().span; // '='
                            self.skip_newlines();
                            let value = self.parse_expr();
                            self.file.value_operator_spans.insert(value.0, operator);
                            return self.finish_assignment_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                    safe: true,
                                },
                                start,
                                target_span,
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
                    let target_span = self.assignment_target_span(e);
                    match self.file.expr(e).clone() {
                        Expr::Name(n) => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_expr();
                            let lhs = self.file.add_expr(Expr::Name(n.clone()), op_span);
                            let value = self.file.add_expr(
                                Expr::Binary {
                                    op,
                                    lhs,
                                    rhs,
                                    operator_span: op_span,
                                },
                                op_span,
                            );
                            return self.finish_assignment_stmt(
                                Stmt::Assign { name: n, value },
                                start,
                                target_span,
                            );
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
                            let value = self.file.add_expr(
                                Expr::Binary {
                                    op,
                                    lhs,
                                    rhs,
                                    operator_span: op_span,
                                },
                                op_span,
                            );
                            return self.finish_assignment_stmt(
                                Stmt::AssignMember {
                                    receiver,
                                    name,
                                    value,
                                    safe: false,
                                },
                                start,
                                target_span,
                            );
                        }
                        Expr::Index { array, indices } => {
                            self.bump();
                            self.skip_newlines();
                            let rhs = self.parse_expr();
                            let lhs = self.file.add_expr(
                                Expr::Index {
                                    array,
                                    indices: indices.clone(),
                                },
                                op_span,
                            );
                            let value = self.file.add_expr(
                                Expr::Binary {
                                    op,
                                    lhs,
                                    rhs,
                                    operator_span: op_span,
                                },
                                op_span,
                            );
                            return self.finish_assignment_stmt(
                                Stmt::AssignIndex {
                                    array,
                                    indices,
                                    value,
                                },
                                start,
                                target_span,
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
        let destructure: Option<DestructureEntries> = if let Some(close) = close {
            self.bump();
            let mut entries = Vec::new();
            // Parallel by-name source properties (`for ((a = prop) in …)`); `None` for a positional entry.
            let mut source_props: Vec<Option<String>> = Vec::new();
            loop {
                // Full form (`for ([val a, val b] in …)`): each component carries its own
                // `val`/`var`. A full-form PAREN component binds by property name; the classic
                // keyword-less short form stays positional (unless the short-form flag is on).
                let is_var = self.at(TokenKind::KwVar);
                let had_kw = is_var || self.at(TokenKind::KwVal);
                if had_kw {
                    self.bump();
                }
                let n = self.ident_or_error("variable name");
                if self.eat(TokenKind::Colon) {
                    let _ = self.parse_type();
                }
                let source = if self.name_based_destructuring && self.eat(TokenKind::Eq) {
                    let src = self.ident_or_error("property name");
                    if self.eat(TokenKind::Colon) {
                        let _ = self.parse_type();
                    }
                    Some(src)
                } else if close == TokenKind::RParen && (self.short_form_destructuring || had_kw) {
                    Some(n.clone())
                } else {
                    None
                };
                entries.push((n, is_var));
                source_props.push(source);
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
            Some((entries, source_props))
        } else {
            None
        };
        let name = match &destructure {
            Some(_) => format!("$dest${}", start.lo),
            None => {
                let n = self.ident_or_error("loop variable");
                // An explicit loop-variable type — `for (i: Int in xs)`. The variable's type is the
                // iterable's element type; the annotation only widens it (`for (c: Char? in str)`), so
                // parse and discard it, mirroring the destructuring path above.
                if self.eat(TokenKind::Colon) {
                    let _ = self.parse_type();
                }
                n
            }
        };
        self.expect(TokenKind::KwIn, "'in'");
        // Parse the iterable / range start at additive precedence so the `..`/`until`/`downTo`
        // operator is left for the `for`-specific range handling below (not swallowed into a
        // `RangeTo` value expression).
        let rstart = self.parse_bp(9);
        let kind = if self.eat(TokenKind::DotDot) {
            RangeKind::Through
        } else if self.eat(TokenKind::DotDotLt) {
            RangeKind::OpenEnd
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
            let mut rstart = self.parse_for_trailing_infix(rstart);
            // The iterable start was parsed at additive precedence (bp 9) so the range operators above
            // stay visible. When it is a plain iterable (no range), lower-precedence operators the bp-9
            // start left behind still belong to it — notably an elvis `?:` (`for (v in foo() ?: continue)`).
            // Fold the elvis chain here so the whole expression becomes the ForEach iterable. Elvis is the
            // loosest operator (below every binop), so nothing below it can precede the `)`; its RHS parses
            // as a full value expression (`parse_elvis`, right-associative, ranges bind tighter).
            while self.at(TokenKind::Question)
                && self
                    .t
                    .get(self.i + 1)
                    .map_or(false, |t| t.kind == TokenKind::Colon)
            {
                self.bump(); // '?'
                self.bump(); // ':'
                self.skip_newlines();
                let rhs = self.parse_elvis();
                let lspan = self.file.expr_spans[rstart.0 as usize];
                let rspan = self.file.expr_spans[rhs.0 as usize];
                rstart = self.file.add_expr(
                    Expr::Elvis { lhs: rstart, rhs },
                    Span::new(lspan.lo, rspan.hi),
                );
            }
            self.expect(TokenKind::RParen, "')'");
            let body = self.parse_loop_body();
            let body = self.desugar_destructure_body(&name, destructure, body);
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
                                    RangeKind::OpenEnd | RangeKind::Until => {
                                        Some((hi, lo, RangeKind::DownTo, true))
                                    }
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
                                                operator_span: sp,
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
            let next = self.infix_operand_follows();
            !matches!(self.text(), "is" | "as" | "in") && next
        } {
            let lspan = self.file.expr_spans[rstart.0 as usize];
            let rspan = self.file.expr_spans[rend.0 as usize];
            let base_span = Span::new(lspan.lo, rspan.hi);
            // Preserve the source declaration kind: only `..`/`..<` are operators. `until` and
            // `downTo` are ordinary infix calls and must enter normal callable resolution.
            let base = match kind {
                RangeKind::Until | RangeKind::DownTo => {
                    let name = match kind {
                        RangeKind::Until => "until",
                        RangeKind::DownTo => "downTo",
                        _ => unreachable!(),
                    };
                    let callee = self.file.add_expr(
                        Expr::Member {
                            receiver: rstart,
                            name: name.to_string(),
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
            let body = self.parse_loop_body();
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
        let body = self.parse_loop_body();
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

    /// Return whether an infix operand follows, allowing newlines after the operator.
    fn infix_operand_follows(&self) -> bool {
        self.t
            .get(self.i + 1..)
            .and_then(|rest| rest.iter().find(|t| t.kind != TokenKind::Newline))
            .is_some_and(|t| starts_expr(t.kind))
    }

    /// Apply trailing infix function calls to a `for`-loop iterable base.
    fn parse_for_trailing_infix(&mut self, mut recv: ExprId) -> ExprId {
        while self.at(TokenKind::Ident) {
            let name = self.text();
            let next_starts_expr = self.infix_operand_follows();
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
        destructure: Option<DestructureEntries>,
        body: ExprId,
    ) -> ExprId {
        let Some((entries, source_props)) = destructure else {
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
        if source_props.iter().any(|s| s.is_some()) {
            self.file
                .destructure_source_props
                .insert(dstmt.0, source_props);
        }
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

    fn assignment_target_span(&self, expression: ExprId) -> Span {
        match self.file.expr(expression) {
            Expr::Member { name, .. } => self
                .file
                .exact_member_name_spans
                .get(&expression.0)
                .copied()
                .unwrap_or_else(|| {
                    let span = self.file.expr_spans[expression.0 as usize];
                    Span::new(span.hi.saturating_sub(name.len() as u32), span.hi)
                }),
            _ => self.file.expr_spans[expression.0 as usize],
        }
    }

    fn finish_assignment_stmt(&mut self, statement: Stmt, start: Span, target: Span) -> StmtId {
        let statement = self.finish_stmt(statement, start);
        self.file
            .assignment_target_spans
            .insert(statement.0, target);
        statement
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
                | Some(TokenKind::Amp)
                | Some(TokenKind::LParen)
                | Some(TokenKind::RParen)
                // `in` is the variance keyword in an argument projection (`Foo<in T>`); `out` is an
                // ordinary ident and already covered above.
                | Some(TokenKind::KwIn)
                | Some(TokenKind::Arrow) => {
                    j += 1;
                }
                _ => return false,
            }
        }
        // After `>`, must be followed by `(`, `{`, `.`, or `::` to be a generic call / a callable
        // reference on a generic type (`A<String>::foo` — the type arguments erase, so it references
        // `A::foo`). Anything else means the `<` was a less-than operator.
        matches!(
            self.t.get(j).map(|t| t.kind),
            Some(TokenKind::LParen)
                | Some(TokenKind::LBrace)
                | Some(TokenKind::Dot)
                | Some(TokenKind::ColonColon)
        )
    }

    fn parse_call_type_args(&mut self) -> Vec<TypeRef> {
        if self.at(TokenKind::Lt) && self.lookahead_is_type_args_call() {
            self.parse_type_args()
        } else {
            Vec::new()
        }
    }

    // ---- expressions (Pratt) ----
    fn parse_expr(&mut self) -> ExprId {
        self.parse_elvis()
    }

    /// Elvis `?:` is the lowest-precedence binary operator (below `||`).
    fn parse_elvis(&mut self) -> ExprId {
        let mut lhs = self.parse_bp(0);
        loop {
            // Elvis may continue on a following line: Kotlin's grammar allows `NL* ?:` (a newline
            // before the operator is part of the elvis expression, not a statement terminator), so
            // `x\n    ?: y` binds as `x ?: y`. Peek past any newlines; consume them and continue only
            // when `?:` actually follows — otherwise the newline stays a statement terminator.
            if self.at(TokenKind::Newline) {
                let mut j = self.i;
                while self.t.get(j).is_some_and(|t| t.kind == TokenKind::Newline) {
                    j += 1;
                }
                let is_elvis = self.t.get(j).is_some_and(|t| t.kind == TokenKind::Question)
                    && self
                        .t
                        .get(j + 1)
                        .is_some_and(|t| t.kind == TokenKind::Colon);
                if !is_elvis {
                    break;
                }
                self.skip_newlines();
            }
            if !(self.at(TokenKind::Question)
                && self
                    .t
                    .get(self.i + 1)
                    .map_or(false, |t| t.kind == TokenKind::Colon))
            {
                break;
            }
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

    /// Depth-guarded entry for the binary-expression parser. Ordinary expression recursion funnels
    /// through here — `parse_expr` → `parse_elvis` → `parse_bp`, and nested operands
    /// (parenthesized/bracketed expressions, call arguments, prefix operands, branch bodies)
    /// re-enter via `parse_expr`. Annotation-value structures recurse outside `parse_bp`, so their
    /// entry explicitly selects the same [`ParserNesting::Expression`] lane. A left-leaning binary
    /// chain (`a && b && c`) iterates in `parse_bp_inner`'s loop and does not grow the depth; only
    /// genuinely nested expressions (`((((x))))`, `f(f(f(x)))`) do. Past [`EXPR_DEPTH_LIMIT`] the
    /// expression degrades to an error expression with a diagnostic — degrade, never crash —
    /// mirroring the checker's and lowering's `expr_depth` guards.
    fn parse_bp(&mut self, min_bp: u8) -> ExprId {
        self.with_nesting_guard(
            ParserNesting::Expression,
            |parser, span| {
                parser
                    .file
                    .add_expr(Expr::Name("<error>".to_string()), span)
            },
            |parser| parser.parse_bp_inner(min_bp),
        )
    }

    /// Apply the parser's depth, recovery, and stack-growth contract to every recursive funnel.
    ///
    /// Keeping the counter lane, diagnostic label, delimiter policy, and per-level stack growth in
    /// one operation prevents a new syntax origin from silently receiving a different safety
    /// policy. Callers provide only their typed degraded node; that is the one aspect which cannot
    /// be shared between an expression, type, statement, and declaration AST.
    fn with_nesting_guard<T>(
        &mut self,
        nesting: ParserNesting,
        degraded: impl FnOnce(&mut Self, crate::diag::Span) -> T,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if *nesting.counter_mut(self) >= EXPR_DEPTH_LIMIT {
            let span = self.tok().span;
            self.diags
                .error(span, format!("{} nesting too deep", nesting.label()));
            self.skip_over_deep_rest(nesting.angle_brackets());
            return degraded(self, span);
        }

        *nesting.counter_mut(self) += 1;
        // Check/grow at EVERY recursive level. A single grown segment was measured to fail before
        // the bound because each semantic level stacks several large unoptimized parser frames.
        // `maybe_grow` is cheap while enough stack remains and chains a segment only near its
        // low-water mark.
        let result = crate::wide_stack::on_wide_stack(|| parse(self));
        *nesting.counter_mut(self) -= 1;
        result
    }

    /// Recovery for a tripped nesting guard: skip the REST of the over-deep construct,
    /// bracket-balanced. Without this, the leftover tokens (`((((…`) re-parse as nesting on the
    /// error node, rebuilding a deep tree that the downstream passes then recurse over — and a
    /// closer-per-frame unwind would emit an `expected ')'`/`'}'` cascade. Stop at a closer or
    /// newline at relative depth 0 (they belong to the enclosing construct, whose frame consumes
    /// them on the way out) or at EOF. With `angle_brackets`, `<`/`>` count as brackets too — in
    /// TYPE position they always are, so each enclosing `parse_type_args` frame finds exactly the
    /// one `>` it expects. A fused `>=` token (`List<Int>= e`, no space) is skipped as "other",
    /// losing a closer — exact parity with the normal-depth parser, whose `parse_type_args` also
    /// fails to see a `>` inside `GtEq`.
    fn skip_over_deep_rest(&mut self, angle_brackets: bool) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                TokenKind::Eof => break,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::Lt if angle_brackets => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                }
                TokenKind::Gt if angle_brackets => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                }
                TokenKind::Newline if depth == 0 => break,
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn parse_bp_inner(&mut self, min_bp: u8) -> ExprId {
        let mut lhs = self.parse_prefix();
        loop {
            // A newline before `||`/`&&`/`?:` is a line continuation, not a terminator — consume it so
            // the operator below (or the enclosing elvis loop, for `?:`) sees it on this logical line.
            self.skip_newlines_before_continuation_op();
            // Prefix operators bind more tightly than casts.
            if min_bp <= BP_CAST && self.at(TokenKind::Ident) && self.text() == "as" {
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
                        Some(RangeKind::OpenEnd)
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
                    Some(RangeKind::OpenEnd)
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
                let next_starts_expr = self.infix_operand_follows();
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
            lhs = self.file.add_expr(
                Expr::Binary {
                    op,
                    lhs,
                    rhs,
                    operator_span: op_span,
                },
                Span::new(lspan.lo, rspan.hi),
            );
        }
        lhs
    }

    fn parse_prefix(&mut self) -> ExprId {
        let start = self.tok().span;
        // Named context parameters on an anonymous function expression:
        // `context(x: C) fun () = …`. `context(...)` remains an ordinary call unless the balanced
        // parameter list is followed by `fun`.
        if self.at(TokenKind::Ident)
            && self.text() == "context"
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|token| token.kind == TokenKind::LParen)
        {
            if let Some(mut after) = self.context_parameter_list_end_at(self.i + 1) {
                while self
                    .t
                    .get(after)
                    .is_some_and(|token| token.kind == TokenKind::Newline)
                {
                    after += 1;
                }
                if self
                    .t
                    .get(after)
                    .is_some_and(|token| token.kind == TokenKind::KwFun)
                {
                    self.bump(); // `context`
                    let context_params = self.parse_param_list();
                    self.skip_newlines();
                    return self.parse_anon_fun(context_params);
                }
            }
        }
        // Expression annotations decorate the following expression and do not change its value:
        // `@Ann fun() { … }`, `@Ann { … }`, `@Ann value`. Parse their arguments through the
        // ordinary annotation grammar, then continue with the same prefix-expression parser so
        // annotations compose with labels, unary operators, and anonymous functions.
        if self.at(TokenKind::At) {
            while self.at(TokenKind::At) {
                self.skip_annotation();
                self.skip_newlines();
            }
            return self.parse_prefix();
        }
        // `throw <expr>` — a soft keyword; raises an exception (bottom type `Nothing`).
        if self.at(TokenKind::Ident) && self.text() == "throw" {
            self.bump(); // 'throw'
            let operand = self.parse_bp(0);
            let end = self.file.expr_spans[operand.0 as usize];
            return self
                .file
                .add_expr(Expr::Throw { operand }, Span::new(start.lo, end.hi));
        }
        // `break`/`continue` (with an optional `@label`) in EXPRESSION position (`m[k] ?: continue`, a
        // `when` arm). Soft keywords (Ident), like `throw`; bottom type `Nothing`. A statement-position
        // `break`/`continue` is handled earlier in `parse_stmt` (→ `Stmt::Break`/`Continue`).
        if self.at(TokenKind::Ident) && (self.text() == "break" || self.text() == "continue") {
            let is_break = self.text() == "break";
            let mut end = self.tok().span; // the `break`/`continue` token
            self.bump(); // 'break' / 'continue'
            let label = if self.at(TokenKind::At) {
                self.bump();
                if self.at(TokenKind::Ident) {
                    let l = self.text().to_string();
                    end = self.tok().span; // the label token
                    self.bump();
                    Some(l)
                } else {
                    None
                }
            } else {
                None
            };
            let sp = Span::new(start.lo, end.hi);
            let e = if is_break {
                Expr::Break { label }
            } else {
                Expr::Continue { label }
            };
            return self.file.add_expr(e, sp);
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
        // Labeled expression prefix: `label@ <expr>` (`l1@ "s"`, `x@ (1L + 2)`, `l@ { … }`). A label
        // names the following expression as a target for a non-local `return@label`/`break@label`; on a
        // plain expression it is a semantic no-op. Detected as `Ident` immediately followed by `@` — the
        // keyword-labels (`break@`, `continue@`, `return@`) are consumed by their handlers above, so any
        // `Ident @` reaching here is an expression label. Consume it and parse the labeled expression
        // (recurse so stacked labels `a@ b@ e` and a following unary/primary all flow through normally).
        // `this`/`super` are also `Ident`s but `this@Outer`/`super@Base` is a labeled RECEIVER, not a
        // labeled expression — leave those for `parse_primary` to bind the `@label` to the receiver.
        if self.at(TokenKind::Ident)
            && !matches!(self.text(), "this" | "super")
            && self
                .t
                .get(self.i + 1)
                .is_some_and(|t| t.kind == TokenKind::At)
        {
            // Consume the WHOLE label chain (`a@ b@ e`) iteratively before the single re-entry:
            // per-label self-recursion here would be an unguarded, un-grown stack path — a long
            // machine-generated label chain must degrade like any other deep nesting, and after
            // this loop the re-entry cannot reach this branch again (the next token is no label).
            let mut labels = Vec::new();
            while self.at(TokenKind::Ident)
                && !matches!(self.text(), "this" | "super")
                && self
                    .t
                    .get(self.i + 1)
                    .is_some_and(|t| t.kind == TokenKind::At)
            {
                labels.push(self.text().to_string());
                self.bump(); // label name
                self.bump(); // '@'
            }
            let labelled = self.parse_prefix();
            self.record_lambda_labels(labelled, labels);
            return labelled;
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

    /// Whether the cursor sits on a chain of `label@` prefixes ending in `{` — a LABELLED TRAILING
    /// LAMBDA. `this`/`super` are excluded: `this@Outer` is a labelled RECEIVER, not a label prefix.
    fn at_labelled_trailing_lambda(&self) -> bool {
        let mut i = self.i;
        let mut seen = false;
        while self.t.get(i).is_some_and(|t| t.kind == TokenKind::Ident)
            && self.t.get(i + 1).is_some_and(|t| t.kind == TokenKind::At)
            && !matches!(self.t[i].text(self.src), "this" | "super")
        {
            seen = true;
            i += 2;
        }
        seen && self.t.get(i).is_some_and(|t| t.kind == TokenKind::LBrace)
    }

    /// Record the explicit label of `expr` when it is a lambda literal. On any other expression a
    /// label is a semantic no-op; on a lambda it REPLACES the implicit callee-name label that a
    /// `return@…` inside the body targets. A chain (`a@ b@ { … }`) keeps the INNERMOST label — the one
    /// written closest to the lambda — since the table holds one label per lambda.
    fn record_lambda_labels(&mut self, expr: ExprId, labels: Vec<String>) {
        if labels.is_empty() || !matches!(self.file.expr(expr), Expr::Lambda { .. }) {
            return;
        }
        for label in labels {
            self.file.lambda_labels.insert(expr.0, label);
        }
    }

    fn parse_postfix(&mut self, mut lhs: ExprId) -> ExprId {
        // Explicit type arguments parsed just before a call paren (`foo<Int>(…)`), attached to the
        // call once it is built so a constructor instantiation (`ArrayList<Int>()`) keeps its args.
        let mut pending_targs: Vec<TypeRef> = Vec::new();
        // Labels consumed just before a trailing lambda (`run outer@{ … }`), attached to the lambda
        // once it is parsed so a `return@outer` inside it finds its target.
        let mut pending_lambda_labels: Vec<String> = Vec::new();
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
                    let name_token = self.tok();
                    let name_span = self.syntactic_ident_span(name_token);
                    let name = self.ident_or_error("member name");
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let type_args = self.parse_call_type_args();
                    let mut safe_names: Vec<Option<String>> = Vec::new();
                    let mut safe_name_spans: Vec<Option<Span>> = Vec::new();
                    let args = if self.at(TokenKind::LParen) {
                        self.bump();
                        let (args, names, name_spans) = self.parse_call_argument_list();
                        self.expect(TokenKind::RParen, "')'");
                        safe_names = names;
                        safe_name_spans = name_spans;
                        Some(args)
                    } else {
                        None
                    };
                    let end = self.t[self.i.saturating_sub(1)].span;
                    let needs_exact_name_span = name_span != name_token.span || args.is_some();
                    let safe_call = self.file.add_expr(
                        Expr::SafeCall {
                            receiver: lhs,
                            name,
                            args,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    if needs_exact_name_span {
                        self.file
                            .exact_member_name_spans
                            .insert(safe_call.0, name_span);
                    }
                    if !type_args.is_empty() {
                        self.file.call_type_args.insert(safe_call.0, type_args);
                    }
                    if safe_names.iter().any(|n| n.is_some()) {
                        self.file.call_arg_names.insert(safe_call.0, safe_names);
                        self.file
                            .call_arg_name_spans
                            .insert(safe_call.0, safe_name_spans);
                    }
                    lhs = safe_call;
                }
                // `Recv?::name` / `Recv?::class` — a callable reference / class literal on a NULLABLE
                // receiver type. The expression still names `Recv`; retain nullability sparsely on the
                // reference node so semantic selection sees `Recv?` rather than silently narrowing it.
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
                    // Type arguments erase from the JVM owner but remain part of the semantic receiver
                    // (`Pair<String, Int>::first` returns `String`). Attach them to the reference node;
                    // an invocation of the resulting function value cannot consume them as call args.
                    let reference = self.file.add_expr(
                        Expr::CallableRef {
                            receiver: Some(lhs),
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    self.file
                        .nullable_callable_ref_receivers
                        .insert(reference.0);
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(reference.0, std::mem::take(&mut pending_targs));
                    }
                    lhs = reference;
                }
                TokenKind::Dot => {
                    let dot_span = self.bump().span;
                    let name_token = self.tok();
                    let name_span = self.syntactic_ident_span(name_token);
                    let name = self.ident_or_error("member name");
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    let member = self.file.add_expr(
                        Expr::Member {
                            receiver: lhs,
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    if name_span != name_token.span {
                        self.file
                            .exact_member_name_spans
                            .insert(member.0, name_span);
                    }
                    if dot_span.hi != name_span.lo {
                        self.file
                            .non_adjacent_member_dot_spans
                            .push((member.0, dot_span));
                    }
                    lhs = member;
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
                    // Preserve the semantic receiver arguments on the reference node. The selected
                    // callable still carries its erased physical owner/descriptor independently.
                    let reference = self.file.add_expr(
                        Expr::CallableRef {
                            receiver: Some(lhs),
                            name,
                        },
                        Span::new(lspan.lo, end.hi),
                    );
                    if !pending_targs.is_empty() {
                        self.file
                            .call_type_args
                            .insert(reference.0, std::mem::take(&mut pending_targs));
                    }
                    lhs = reference;
                }
                TokenKind::LParen => {
                    let open_paren = self.bump().span;
                    let (args, names, name_spans) = self.parse_call_argument_list();
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.expect(TokenKind::RParen, "')'");
                    let is_empty = args.is_empty();
                    let call = self.file.add_expr(
                        Expr::Call { callee: lhs, args },
                        Span::new(lspan.lo, end.hi),
                    );
                    if is_empty {
                        self.file
                            .empty_call_open_paren_spans
                            .insert(call.0, open_paren);
                    }
                    if names.iter().any(|n| n.is_some()) {
                        self.file.call_arg_names.insert(call.0, names);
                        self.file.call_arg_name_spans.insert(call.0, name_spans);
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
                // A trailing lambda may carry an explicit LABEL (`run outer@{ … }`, `xs.sumOf s@{ … }`)
                // naming it as the target of a `return@outer` inside. Consume the label chain here so
                // the `{` still attaches to the call; without it the callee stayed a bare name and the
                // labelled block became a separate statement ("unresolved reference 'run'").
                TokenKind::Ident
                    if !self.no_trailing_lambda && self.at_labelled_trailing_lambda() =>
                {
                    while self.at(TokenKind::Ident)
                        && self
                            .t
                            .get(self.i + 1)
                            .is_some_and(|t| t.kind == TokenKind::At)
                    {
                        pending_lambda_labels.push(self.text().to_string());
                        self.bump(); // label name
                        self.bump(); // '@'
                    }
                }
                TokenKind::LBrace => {
                    let lambda = self.parse_lambda();
                    self.record_lambda_labels(lambda, std::mem::take(&mut pending_lambda_labels));
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.t[self.i.saturating_sub(1)].span;
                    let old = lhs;
                    let has_named_parenthesized_args =
                        self.file.call_arg_names.contains_key(&old.0);
                    let close_paren_end = match (self.file.expr(old), has_named_parenthesized_args)
                    {
                        (Expr::Call { .. } | Expr::SafeCall { args: Some(_), .. }, true) => {
                            Some(self.file.expr_spans[old.0 as usize].hi)
                        }
                        _ => None,
                    };
                    let rebuilt_safe_call_name_span =
                        if let Expr::SafeCall { name, .. } = self.file.expr(old) {
                            self.file
                                .exact_member_name_spans
                                .get(&old.0)
                                .copied()
                                .or_else(|| {
                                    let span = self.file.expr_spans[old.0 as usize];
                                    Some(Span::new(
                                        span.hi.saturating_sub(name.len() as u32),
                                        span.hi,
                                    ))
                                })
                        } else {
                            None
                        };
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
                    if let Some(mut spans) = self.file.call_arg_name_spans.remove(&old.0) {
                        spans.push(None);
                        self.file.call_arg_name_spans.insert(lhs.0, spans);
                    }
                    // The rebuilt call contains the trailing lambda, so it is no longer empty.
                    self.file.empty_call_open_paren_spans.remove(&old.0);
                    // A safe call is itself rebuilt to append the lambda; move its exact member-name
                    // location to the live expression ID. For an ordinary member, `old` remains the
                    // rebuilt `Call`'s callee and must keep its own metadata.
                    if let Some(span) = rebuilt_safe_call_name_span {
                        self.file.exact_member_name_spans.remove(&old.0);
                        self.file.exact_member_name_spans.insert(lhs.0, span);
                    }
                    // Mark this call as having a SYNTACTIC trailing lambda so default-omission lowering
                    // binds it to the callee's LAST parameter (preceding gaps take defaults).
                    self.file.call_has_trailing_lambda.remove(&old.0);
                    self.file.call_has_trailing_lambda.insert(lhs.0);
                    if let Some(close_paren_end) = close_paren_end {
                        self.file
                            .trailing_call_close_paren_ends
                            .insert(lhs.0, close_paren_end);
                    }
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
                // `array[index]` element access, or `receiver[i, j, …]` — a multi-index `get` operator.
                TokenKind::LBracket => {
                    self.bump();
                    self.skip_newlines();
                    let mut indices = vec![self.parse_expr()];
                    self.skip_newlines();
                    while self.eat(TokenKind::Comma) {
                        self.skip_newlines();
                        if self.at(TokenKind::RBracket) {
                            break; // tolerate a trailing comma
                        }
                        indices.push(self.parse_expr());
                        self.skip_newlines();
                    }
                    let lspan = self.file.expr_spans[lhs.0 as usize];
                    let end = self.tok().span;
                    self.expect(TokenKind::RBracket, "']'");
                    let span = Span::new(lspan.lo, end.hi);
                    lhs = self.file.add_expr(
                        Expr::Index {
                            array: lhs,
                            indices,
                        },
                        span,
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
            TokenKind::TemplateStart | TokenKind::RawTemplateStart => self.parse_template(),
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
            // A suspending lambda literal `suspend { … }` (in value position): the `suspend`
            // modifier marks the following lambda — recorded in a side-table so the checker types
            // it as a `suspend (…) -> …` function type and the lowerer builds a SuspendLambda
            // state machine for it (otherwise it misparses as a call to a fn named `suspend`).
            TokenKind::Ident
                if self.text() == "suspend"
                    && self
                        .t
                        .get(self.i + 1)
                        .is_some_and(|t| t.kind == TokenKind::LBrace) =>
            {
                self.bump(); // 'suspend'
                let lambda = self.parse_lambda();
                self.file.suspend_lambdas.insert(lambda.0);
                lambda
            }
            TokenKind::Ident => {
                let mut n = self.text().to_string();
                self.bump();
                // A TYPED super (`super<Base>.foo()`): the `<Base>` type qualifier selects WHICH
                // supertype's method to dispatch to. Encode it on the name (`super<Base>`) so the
                // checker/lowerer pick that interface's default; may be followed by a `@label`.
                if n == "super" && self.at(TokenKind::Lt) {
                    self.bump(); // '<'
                    let ty = self.parse_qualified_name();
                    let simple = ty.rsplit('.').next().unwrap_or(&ty).to_string();
                    self.expect(TokenKind::Gt, "'>'");
                    n = format!("super<{simple}>");
                }
                // A LABELED `this`/`super` (`this@Outer`, `super@Base`): the `@label` qualifies which
                // enclosing receiver / supertype it denotes. Capture it on the name (`this@Outer`) so the
                // checker/lowerer can resolve the label; a bare `this`/`super` stays unchanged.
                if (n == "this" || n.starts_with("super"))
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
            // Anonymous function expression: `fun (params): T = expr` / `fun (params): T { … }`. It
            // desugars to a lambda carrying explicit parameter types; a bare `return` in the block body
            // returns from the anonymous function, exactly as it does from a lambda compiled to its own
            // `invoke`. The receiver form `fun R.(…)` is not desugared here yet.
            TokenKind::KwFun => self.parse_anon_fun(Vec::new()),
            // `::name` — top-level callable reference / class literal without a receiver.
            TokenKind::ColonColon => {
                self.bump(); // '::'
                let name_token = self.tok();
                let name_span = self.syntactic_ident_span(name_token);
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
                let reference = self.file.add_expr(
                    Expr::CallableRef {
                        receiver: None,
                        name,
                    },
                    Span::new(span.lo, end.hi),
                );
                self.file
                    .exact_member_name_spans
                    .insert(reference.0, name_span);
                reference
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
        // The condition may start (and end) on a fresh line: `if(\n  a && b\n)`. Skip newlines around it.
        self.skip_newlines();
        let cond = self.parse_expr();
        self.skip_newlines();
        self.expect(TokenKind::RParen, "')'");
        self.skip_newlines();
        let then_branch = self.parse_branch(true);
        // optional else (may be on the next line)
        let save = self.i;
        self.skip_newlines();
        // An `if` used as a `when`-branch body without its own else (`when { x -> if (c) a; else -> b }`)
        // must NOT swallow the `when`'s `else` entry: an `else` immediately followed by `->` is a when
        // entry, not this `if`'s else (a real if-else branch never begins with `->`).
        let else_is_when_entry = self.at(TokenKind::KwElse) && {
            let mut j = self.i + 1;
            while self.t.get(j).is_some_and(|t| t.kind == TokenKind::Newline) {
                j += 1;
            }
            self.t.get(j).is_some_and(|t| t.kind == TokenKind::Arrow)
        };
        let else_branch = if !else_is_when_entry && self.eat(TokenKind::KwElse) {
            self.skip_newlines();
            Some(self.parse_branch(true))
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
        if self.text().starts_with('$') && !self.multi_dollar_interpolation {
            self.diags.error(
                start,
                "multi-dollar string interpolation is disabled by the language feature set",
            );
        }
        // A raw (triple-quoted) template's chunks are verbatim — no escape processing.
        let raw = self.kind() == TokenKind::RawTemplateStart;
        self.bump(); // TemplateStart / RawTemplateStart
        let mut parts = Vec::new();
        loop {
            match self.kind() {
                TokenKind::StrChunk => {
                    let text = self.text();
                    let piece = if raw {
                        KtString::from(text)
                    } else {
                        unescape_chunk(text)
                    };
                    parts.push(TemplatePart::Str(piece));
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
        let body = self.parse_block_expr(true);
        let mut catches = Vec::new();
        let mut finally = None;
        loop {
            let save = self.i;
            self.skip_newlines();
            if self.at(TokenKind::Ident) && self.text() == "catch" {
                self.bump(); // 'catch'
                self.expect(TokenKind::LParen, "'('");
                // The parameter may sit on its own line(s) inside the parens (`catch (\n e: E\n)`),
                // so skip newlines around each part exactly as an ordinary parameter list allows.
                self.skip_newlines();
                // Annotations on the catch parameter (`catch (@Marker e: E)`): consume and discard —
                // a catch parameter is never referenced by annotation, so its markers carry no codegen.
                while self.at(TokenKind::At) {
                    self.skip_annotation();
                    self.skip_newlines();
                }
                let name = self.ident_or_error("catch parameter name");
                self.skip_newlines();
                self.expect(TokenKind::Colon, "':'");
                self.skip_newlines();
                let ty = self.parse_type();
                self.skip_newlines();
                // A trailing comma is allowed (`catch (e: E,)`), matching Kotlin's parameter lists.
                if self.eat(TokenKind::Comma) {
                    self.skip_newlines();
                }
                self.expect(TokenKind::RParen, "')'");
                self.skip_newlines();
                let cbody = self.parse_block_expr(true);
                catches.push(CatchClause {
                    name,
                    ty,
                    body: cbody,
                });
            } else if self.at(TokenKind::Ident) && self.text() == "finally" {
                self.bump(); // 'finally'
                self.skip_newlines();
                finally = Some(self.parse_block_expr(false));
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
    /// Build `receiver.inc()` / `receiver.dec()` — the desugar of `receiver++`/`receiver--` for a
    /// member/index target (works for the built-in numeric operators and a user `inc`/`dec`).
    fn build_inc_dec_call(&mut self, receiver: ExprId, op_name: &str, span: Span) -> ExprId {
        let callee = self.file.add_expr(
            Expr::Member {
                receiver,
                name: op_name.to_string(),
            },
            span,
        );
        self.file.add_expr(
            Expr::Call {
                callee,
                args: Vec::new(),
            },
            span,
        )
    }

    /// Whether a member/index target can use the current assignment AST without evaluating its
    /// receiver or indices more than once. Name targets have their dedicated `Stmt::IncDec` path;
    /// every other expression is rejected consistently by statement and value inc/dec handling.
    fn incdec_access_is_pure(&self, target: ExprId) -> bool {
        match self.file.expr(target) {
            Expr::Member { receiver, .. } => self.is_pure_path(*receiver),
            Expr::Index { array, indices } => {
                self.is_pure_path(*array) && indices.iter().all(|&i| self.is_pure_path(i))
            }
            _ => false,
        }
    }

    /// Build the store half shared by discarded-value and value-producing member/index inc/dec.
    /// Centralizing this match keeps the accepted lvalue families, source spans, and future property
    /// or index-store extensions identical across both syntactic contexts.
    fn finish_incdec_access_assignment(
        &mut self,
        target: ExprId,
        value: ExprId,
        start: Span,
        target_span: Span,
    ) -> Option<StmtId> {
        let statement = match self.file.expr(target).clone() {
            Expr::Member { receiver, name } => Stmt::AssignMember {
                receiver,
                name,
                value,
                safe: false,
            },
            Expr::Index { array, indices } => Stmt::AssignIndex {
                array,
                indices,
                value,
            },
            _ => return None,
        };
        Some(self.finish_assignment_stmt(statement, start, target_span))
    }

    fn incdec_target(
        &mut self,
        e: ExprId,
        dec: bool,
        prefix: bool,
        op_span: Span,
        start: Span,
    ) -> StmtId {
        // The desugar `target = target.inc()` re-evaluates `target`, so its receiver/index must be
        // side-effect-free (a pure access path). For a complex receiver (`f().x++`) kotlinc evaluates
        // it exactly once — not yet modeled — so bail (skip the file) rather than double-evaluate.
        // `.inc()`/`.dec()` covers both the built-in numeric operators and a user `inc`/`dec` operator.
        let op_name = if dec { "dec" } else { "inc" };
        let target_span = self.assignment_target_span(e);
        match self.file.expr(e).clone() {
            Expr::Name(n) => self.parse_incdec(n, dec, prefix, start, target_span),
            Expr::Member { .. } | Expr::Index { .. } if self.incdec_access_is_pure(e) => {
                let lhs = self.file.add_expr(self.file.expr(e).clone(), op_span);
                let value = self.build_inc_dec_call(lhs, op_name, op_span);
                self.finish_incdec_access_assignment(e, value, start, target_span)
                    .expect("pure member/index target has an assignment form")
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
            Expr::Index { array, indices } => {
                self.is_pure_path(*array) && indices.iter().all(|&i| self.is_pure_path(i))
            }
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
            // The subject (or subject-variable binding) may start on a fresh line: `when(\n  val v = e\n)`.
            self.skip_newlines();
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
                // A `when` subject initializer may start on the next line.
                self.skip_newlines();
                let init = self.parse_expr();
                self.skip_newlines();
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
                self.skip_newlines();
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
                    if subject.is_some() && self.at(TokenKind::Arrow) {
                        break;
                    }
                    conditions.push(self.parse_when_condition(subject));
                }
            }
            let guard = if self.eat(TokenKind::KwIf) {
                if !self.when_guards {
                    self.diags.error(
                        self.t[self.i.saturating_sub(1)].span,
                        "when guards are disabled by the language feature set",
                    );
                }
                Some(self.parse_expr())
            } else {
                None
            };
            self.expect(TokenKind::Arrow, "'->'");
            self.skip_newlines();
            let body = self.parse_branch(true);
            arms.push(WhenArm {
                conditions,
                guard,
                body,
            });
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
                Some(RangeKind::OpenEnd)
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
    /// At a `{`, whether it opens a LAMBDA (a top-level `->` precedes the matching `}`) rather than a
    /// block. Used to disambiguate a lambda branch body from a statement block.
    fn at_lambda_brace(&self) -> bool {
        self.lambda_arrow_before_close(self.i + 1)
    }

    /// Whether the supertype entry at the current position is a FUNCTION TYPE (`() -> R`, `(A) -> R`,
    /// `Recv.() -> R`, `suspend (A) -> R`) rather than a class/interface name — detected by a depth-0
    /// `->` before the entry's terminator (`,`, `{`, `by`/`where`, or end). A regular supertype (even
    /// a generic `Base<A>` or a base-class call `Base(args)`) has no top-level `->` (a lambda argument's
    /// arrow sits inside `(`/`{`, at depth > 0), so this cleanly distinguishes the two.
    fn at_function_type_supertype(&self) -> bool {
        let mut j = self.i;
        let mut depth = 0i32;
        loop {
            match self.t.get(j).map(|t| t.kind) {
                None => return false,
                Some(TokenKind::Arrow) if depth == 0 => return true,
                Some(TokenKind::LBrace) if depth == 0 => return false, // class body
                // A closing `}` at depth 0 is the END of the enclosing class body — the supertype
                // clause is over. Stop here rather than decrementing into negative depth and scanning
                // on into the NEXT top-level declaration, where an unrelated `->` (a `when` arm or a
                // lambda) would be misread as this supertype's function-type arrow.
                Some(TokenKind::RBrace) if depth == 0 => return false,
                Some(TokenKind::Comma) if depth == 0 => return false, // next supertype
                // Track generic-argument brackets too (`<` is a single `Lt`, `>` a single `Gt`), so a
                // function type used as a type ARGUMENT — `Base<() -> R>` — keeps its `->` at depth > 0
                // and is NOT mistaken for a function-type supertype.
                Some(
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace | TokenKind::Lt,
                ) => depth += 1,
                Some(
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace | TokenKind::Gt,
                ) => depth -= 1,
                // `by` (delegation) / `where` (constraints) terminate a plain-name supertype entry.
                Some(TokenKind::Ident)
                    if depth == 0 && matches!(self.t[j].text(self.src), "by" | "where") =>
                {
                    return false
                }
                _ => {}
            }
            j += 1;
        }
    }

    /// Whether the balanced parenthesized group starting at token index `at` (which must be `(`) is
    /// immediately followed by `->` — i.e. it is a function type's parameter list, not an annotation
    /// argument list. Used to disambiguate `@Ann("a") String` (annotation args) from
    /// `@Composable () -> Unit` (function type).
    fn paren_group_precedes_arrow(&self, at: usize) -> bool {
        let mut j = at;
        let mut depth = 0i32;
        loop {
            match self.t.get(j).map(|t| t.kind) {
                None => return false,
                Some(TokenKind::LParen) => depth += 1,
                Some(TokenKind::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return self
                            .t
                            .get(j + 1)
                            .is_some_and(|t| t.kind == TokenKind::Arrow);
                    }
                }
                _ => {}
            }
            j += 1;
        }
    }

    /// Whether a top-level `->` (a lambda's parameter arrow) precedes the matching `}`, scanning from
    /// token index `from`. Distinguishes a lambda `{ p -> … }` from a statement block. A lambda's
    /// parameter list (everything before its arrow) is only names, `:`, commas, destructuring parens,
    /// and parameter TYPES — never a `val`/`var`/`=`/statement keyword. Hitting one at depth 0 before
    /// any arrow means the arrow belongs to a nested function TYPE inside a statement
    /// (`{ val u: (Int) -> Unit = … }`), so this is a BLOCK, not a lambda.
    fn lambda_arrow_before_close(&self, from: usize) -> bool {
        let mut j = from;
        let mut depth = 0i32;
        loop {
            match self.t.get(j).map(|t| t.kind) {
                None => return false,
                Some(
                    TokenKind::KwVal
                    | TokenKind::KwVar
                    | TokenKind::KwReturn
                    | TokenKind::KwWhile
                    | TokenKind::KwFor
                    | TokenKind::KwDo
                    | TokenKind::KwIf
                    | TokenKind::KwWhen
                    | TokenKind::KwClass
                    | TokenKind::KwFun
                    | TokenKind::KwImport
                    | TokenKind::KwPackage
                    | TokenKind::Eq,
                ) if depth == 0 => return false,
                // An `object` expression (`{ object : () -> Unit { … } }`): `object` is a hard keyword,
                // never a lambda parameter name, so a `->` reached inside its supertype/body belongs to a
                // function-type supertype — the brace is a BLOCK, not a lambda.
                Some(TokenKind::Ident) if depth == 0 && self.t[j].text(self.src) == "object" => {
                    return false
                }
                Some(TokenKind::Arrow) if depth == 0 => return true,
                Some(TokenKind::RBrace) if depth == 0 => return false,
                Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace) => depth += 1,
                Some(TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace) => depth -= 1,
                _ => {}
            }
            j += 1;
        }
    }

    fn parse_branch(&mut self, trailing_is_value: bool) -> ExprId {
        if self.at(TokenKind::LBrace) {
            // A branch body `{ … }` is a BLOCK — unless it is a LAMBDA (`when (x) { … -> { _ -> body } }`
            // returning a function type), detected by a top-level `->` before the closing `}`.
            if self.at_lambda_brace() {
                return self.parse_lambda();
            }
            return self.parse_block_expr(trailing_is_value);
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
const BP_CAST: u8 = 12;
const BP_PREFIX: u8 = 13;

/// The visibility a declaration's modifier list denotes — the first visibility keyword present, or
/// `public` (Kotlin's default) when none is written.
fn visibility_of(mods: &[String]) -> crate::types::Visibility {
    mods.iter()
        .find(|m| matches!(m.as_str(), "private" | "protected" | "internal" | "public"))
        .map(|m| crate::types::Visibility::from_modifier(m))
        .unwrap_or_default()
}

fn function_flags(modifiers: &[String]) -> FdFlags {
    let mut flags = FdFlags::default();
    let mut is_final = false;
    let mut is_open = false;
    for modifier in modifiers {
        flags = match modifier.as_str() {
            "inline" => flags.with_is_inline(true),
            "final" => {
                is_final = true;
                flags.with_is_final(true)
            }
            "open" => {
                is_open = true;
                flags
            }
            "override" => {
                is_open = true;
                flags.with_is_override(true)
            }
            "abstract" => flags.with_is_abstract(true),
            "suspend" => flags.with_is_suspend(true),
            "tailrec" => flags.with_is_tailrec(true),
            "operator" => flags.with_is_operator(true),
            "infix" => flags.with_is_infix(true),
            _ => flags,
        };
    }
    flags.with_is_open(is_open && !is_final)
}

/// Soft modifiers that don't change a declaration's *kind* (so krusty can ignore them). Excludes
/// `data`/`enum`/`annotation`/`value`/`object`/`companion`/`inner`/`expect`/`actual`,
/// which would alter parsing/semantics and must remain unsupported. `sealed` is included: it maps
/// cleanly onto an abstract, open class (see the top-level dispatch), so ignoring its
/// exhaustiveness aspect never miscompiles.
/// A parenthesised/bracketed destructuring's variable entries `(name, is_var)` paired with each
/// entry's optional by-name source property (parallel; `None` = positional `componentN`).
type DestructureEntries = (Vec<(String, bool)>, Vec<Option<String>>);

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
            | TokenKind::RawTemplateStart
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

/// Decode a lexer-validated `'x'` token to its UTF-16 code unit.
///
/// Validation and decoding deliberately share the token-layer contract. The fallback is parser
/// recovery only: the lexer has already diagnosed malformed content, but the AST still needs a
/// deterministic placeholder so later syntax recovery can continue without another escape table.
fn unquote_char(raw: &str) -> u16 {
    let inner = raw
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(raw);
    decode_char_literal_content(inner).unwrap_or(0)
}

/// Unescape a literal chunk of a string template (no surrounding quotes).
fn unescape_chunk(inner: &str) -> KtString {
    let mut out = KtStringBuf::with_capacity(inner.len());
    unescape_into(inner, &mut out);
    out.finish()
}

/// Unescape `inner` (a string body without its quotes) into `out`.
///
/// `\uXXXX` denotes one UTF-16 code UNIT and is taken verbatim, including the surrogate range
/// D800..DFFF: `"😀"` is U+1F600 written as its two halves, and `"\uD800"` is a legal
/// one-element string. Decoding each escape through `char::from_u32` rejects both, which is why the
/// accumulator is a [`KtStringBuf`] rather than a `String`.
fn unescape_into(inner: &str, out: &mut KtStringBuf) {
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
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(unit) = u32::from_str_radix(&hex, 16) {
                        out.push_unit(unit as u16);
                    }
                }
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
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

fn unquote(raw: &str) -> KtString {
    // Raw string `"""..."""`: content is verbatim (no escape processing), three quotes each side.
    if raw.starts_with("\"\"\"") {
        let inner = raw
            .strip_prefix("\"\"\"")
            .and_then(|s| s.strip_suffix("\"\"\""))
            .unwrap_or(raw);
        return KtString::from(inner);
    }
    let inner = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    let mut out = KtStringBuf::with_capacity(inner.len());
    unescape_into(inner, &mut out);
    out.finish()
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

    fn script(src: &str) -> File {
        let mut diagnostics = DiagSink::new();
        let tokens = lex(src, &mut diagnostics);
        let file = parse_script_with_features(src, &tokens, &mut diagnostics, &LangFeatures::new());
        assert!(
            !diagnostics.has_errors(),
            "{}",
            diagnostics.render("test", src)
        );
        file
    }

    #[test]
    fn every_vararg_type_ref_keeps_the_written_element_type() {
        let source = "fun top(vararg values: String) {}\n\
                      class C(vararg val primary: String) {\n\
                          constructor(vararg secondary: Int) : this()\n\
                      }";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "{}",
            diagnostics.render("test", source)
        );
        let top = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Fun(function) => Some(function),
                _ => None,
            })
            .expect("top-level function");
        let class = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) => Some(class),
                _ => None,
            })
            .expect("class");

        assert_eq!(top.params[0].ty.name, "String");
        assert_eq!(class.props[0].ty.name, "String");
        assert_eq!(class.secondary_ctors[0].params[0].ty.name, "Int");
        assert!(top.params[0].is_vararg);
        assert!(class.props[0].is_vararg);
        assert!(class.secondary_ctors[0].params[0].is_vararg);
    }

    #[test]
    fn script_parses_declarations_and_modifier_shaped_calls_in_order() {
        let source = "fun context(value: String): String = value\n\
                      fun suspend(block: () -> Unit) = block()\n\
                      context(\"first\")\n\
                      context(\"second\"); fun after(): Unit {}\n\
                      suspend {}";
        let file = script(source);
        let Some(Expr::Block { stmts, .. }) = file.script_body.map(|body| file.expr(body)) else {
            panic!("script body");
        };
        let declaration_names = stmts
            .iter()
            .filter_map(|statement| match file.stmt(*statement) {
                Stmt::LocalFun(function) => Some(function.name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(file.decls.is_empty());
        assert_eq!(declaration_names, ["context", "suspend", "after"]);
        assert_eq!(stmts.len(), 6);
    }

    #[test]
    fn script_parses_prefixed_declarations_as_local_statements() {
        let source = "@Deprecated(\"test\")\n\
                      private fun hidden(): Unit {}\n\
                      context(value: String)\n\
                      private fun scoped(): String = value\n\
                      scoped()";
        let file = script(source);

        let Some(Expr::Block { stmts, .. }) = file.script_body.map(|body| file.expr(body)) else {
            panic!("script body");
        };
        let functions = stmts
            .iter()
            .filter_map(|statement| match file.stmt(*statement) {
                Stmt::LocalFun(function) => Some(function),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(file.decls.is_empty());
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].visibility, Visibility::Private);
        assert_eq!(functions[1].context_count, 1);
        assert_eq!(stmts.len(), 3);
    }

    #[test]
    fn script_distinguishes_context_declarations_from_calls() {
        let source = "fun context(value: String) = value\n\
                      context(\"call\")\n\
                      context(value: String,)\n\
                      private fun declared() = value\n\
                      fun () {}";
        let file = script(source);
        let Some(Expr::Block { stmts, .. }) = file.script_body.map(|body| file.expr(body)) else {
            panic!("script body");
        };

        assert_eq!(stmts.len(), 4);
        assert!(matches!(file.stmt(stmts[1]), Stmt::Expr(_)));
        assert!(matches!(
            file.stmt(stmts[2]),
            Stmt::LocalFun(function) if function.context_count == 1
        ));
        assert!(matches!(file.stmt(stmts[3]), Stmt::Expr(_)));
    }

    #[test]
    fn diagnostic_operator_and_named_argument_locations_are_sparse_spans() {
        let mut diagnostics = DiagSink::new();
        let source = "fun target(`a`: Int = 0, b: Int) = a + b\n\
                      fun use(x: String) { var y: String = \"\"; y = 1; \
                      target<Int> (); target(`a` = 1); x.`missing name`; \
                      x?.`missing safe` { }; x?.missing { } }\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("test", source)
        );

        assert_eq!(file.value_operator_spans.len(), 3);
        for span in file.value_operator_spans.values() {
            assert_eq!(&source[span.lo as usize..span.hi as usize], "=");
        }
        let binary_operators = file
            .expr_arena
            .iter()
            .filter_map(|expression| match expression {
                Expr::Binary { operator_span, .. } => Some(operator_span),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(binary_operators.len(), 1);
        assert_eq!(
            &source[binary_operators[0].lo as usize..binary_operators[0].hi as usize],
            "+"
        );
        let labels = file
            .call_arg_name_spans
            .values()
            .flat_map(|spans| spans.iter().flatten())
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 1);
        assert_eq!(&source[labels[0].lo as usize..labels[0].hi as usize], "`a`");
        let open_parens = file
            .empty_call_open_paren_spans
            .values()
            .collect::<Vec<_>>();
        assert_eq!(open_parens.len(), 1);
        assert_eq!(
            &source[open_parens[0].lo as usize..open_parens[0].hi as usize],
            "("
        );
        let mut exact_members = file
            .exact_member_name_spans
            .values()
            .map(|span| &source[span.lo as usize..span.hi as usize])
            .collect::<Vec<_>>();
        exact_members.sort_unstable();
        assert_eq!(
            exact_members,
            vec!["`missing name`", "`missing safe`", "missing"],
            "rebuilt safe calls must each have one live exact-name entry"
        );
    }

    #[test]
    fn unparseable_construct_does_not_poison_siblings() {
        // An unsupported top-level construct (here a stray `init` block) must produce ONE error and skip
        // to the next declaration — NOT mis-token keyword-by-keyword and drift into / swallow the
        // sibling declaration (the reported `unresolved reference 'private'/'suspend'` cascade).
        let mut d = DiagSink::new();
        let src = "init { }\nfun f() = 1\nclass Sibling(val n: String)\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        let sibling_survives = file
            .decls
            .iter()
            .any(|&id| matches!(file.decl(id), Decl::Class(c) if c.name == "Sibling"));
        assert!(sibling_survives, "recovery swallowed the sibling class");
        let errors = d
            .diags
            .iter()
            .filter(|x| x.severity == crate::diag::Severity::Error)
            .count();
        assert_eq!(errors, 1, "cascade: {}", d.render("t", src));
    }

    #[test]
    fn context_receiver_parses_as_leading_params() {
        // `context(a: A) fun f()` parses: the context parameters become LEADING value parameters and
        // `context_count` records how many. `context` stays usable as an ordinary identifier elsewhere.
        let mut d = DiagSink::new();
        let src = "class A\ncontext(a: A)\nfun f() { }\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "unexpected: {}", d.render("t", src));
        let f = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Fun(f) if f.name == "f" => Some(f),
                _ => None,
            })
            .expect("fun f parsed");
        assert_eq!(f.context_count, 1);
        assert_eq!(f.params.len(), 1);
        assert_eq!(f.params[0].name, "a");
        assert_eq!(f.params[0].ty.name, "A");
        // `context` as an ordinary identifier (a function name / value) is unaffected.
        assert!(!tree("fun context(): Int = 1").is_empty());
    }

    #[test]
    fn generic_qualified_nested_type_parses() {
        // `Outer<A>.Inner<B>.Innermost<C>` — a nested type whose OUTER segments carry type arguments.
        // The `<A>` breaks the plain dotted-path scan, so the parser must keep consuming `.Nested`
        // segments (each with its own erased arguments), yielding the dotted name for resolution.
        let mut d = DiagSink::new();
        let src =
            "fun foo(): Outer<Int, Number>.Inner<String, Float>.Innermost<Any, Any?> = null!!\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "unexpected: {}", d.render("t", src));
        let f = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Fun(f) if f.name == "foo" => Some(f),
                _ => None,
            })
            .expect("fun foo parsed");
        let ret = f.ret.as_ref().expect("return type");
        assert_eq!(ret.name, "Outer.Inner.Innermost");
        // Only the last segment's (erased) arguments are retained.
        assert_eq!(ret.targs.len(), 2);
    }

    #[test]
    fn extension_receivers_keep_type_arguments() {
        let mut diagnostics = DiagSink::new();
        let source = "class Entry\n\
                      class Outer { class Inner<T>; class Array<T> }\n\
                      fun Collection<Entry>.selected() {}\n\
                      val Collection<Entry>.counted: Int get() = 0\n\
                      fun Outer.Inner<Entry>.dottedFunction() {}\n\
                      val Outer.Inner<Entry>.dottedProperty: Int get() = 0\n\
                      fun (() -> Unit).parenthesizedFunction() {}\n\
                      val (() -> Unit).parenthesizedProperty: Int get() = 0\n\
                      fun Outer.Array<Entry> /* gap */ .qualifiedArray() {}\n\
                      fun Array<Entry> /* gap */ .builtinArray() {}\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("t", source)
        );
        let receivers: Vec<_> = file
            .decls
            .iter()
            .filter_map(|&id| match file.decl(id) {
                Decl::Fun(function) => function.receiver.as_ref(),
                Decl::Property(property) => property.receiver.as_ref(),
                Decl::Class(_) => None,
            })
            .collect();
        assert_eq!(receivers.len(), 8);
        assert_eq!(
            receivers
                .iter()
                .map(|receiver| { &source[receiver.span.lo as usize..receiver.span.hi as usize] })
                .collect::<Vec<_>>(),
            [
                "Collection<Entry>",
                "Collection<Entry>",
                "Outer.Inner<Entry>",
                "Outer.Inner<Entry>",
                "(() -> Unit)",
                "(() -> Unit)",
                "Outer.Array<Entry>",
                "Array<Entry>"
            ]
        );
        assert_eq!(receivers[0].name, "Collection");
        assert_eq!(receivers[0].targs[0].name, "Entry");
        assert_eq!(receivers[2].name, "Outer.Inner");
        assert_eq!(receivers[2].targs[0].name, "Entry");
        assert_eq!(receivers[6].name, "Outer.Array");
        assert_eq!(receivers[6].targs[0].name, "Entry");
        assert!(receivers[6].arg.is_none());
        assert_eq!(receivers[7].name, "Array");
        assert!(receivers[7].arg.is_none());
        assert_eq!(receivers[7].targs[0].name, "Entry");
    }

    #[test]
    fn use_site_variance_is_preserved_on_type_arguments() {
        let mut diagnostics = DiagSink::new();
        let source = "class Context<T>\n\
                      fun <T> projected(input: Context<in T>, output: Context<out T>, array: Array<in T>) {}";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("t", source)
        );
        let function = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Fun(function) if function.name == "projected" => Some(function),
                _ => None,
            })
            .expect("projected function");
        let input = &function.params[0].ty.targs[0];
        let output = &function.params[1].ty.targs[0];
        let array = &function.params[2].ty;
        assert!(input.in_projection());
        assert!(!input.out_projection());
        assert!(!output.in_projection());
        assert!(output.out_projection());
        assert!(array.arg.is_none());
        let projected = &array.targs[0];
        assert!(projected.in_projection());
        assert_eq!(projected.name, "T");
    }

    #[test]
    fn star_projection_keeps_its_source_identity_after_bound_erasure() {
        // Both arguments resolve as nullable Any for ordinary type reasoning, but only the first was
        // written as `*`. Runtime `is FunctionN<*, ...>` validation needs that distinction and must
        // not infer it from the erased name/nullability pair.
        let mut diagnostics = DiagSink::new();
        let source = "fun projected(value: Function1<*, Any?>) {}";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("t", source)
        );
        let function = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Fun(function) if function.name == "projected" => Some(function),
                _ => None,
            })
            .expect("projected function");
        let arguments = &function.params[0].ty.targs;
        assert_eq!(arguments.len(), 2);
        assert!(arguments.iter().all(|argument| argument.name == "Any"));
        assert!(arguments.iter().all(TypeRef::nullable));
        assert!(arguments[0].is_star_projection());
        assert!(!arguments[1].is_star_projection());
    }

    #[test]
    fn context_prefix_keeps_trailing_modifiers() {
        // A modifier written AFTER the context prefix (`context(a: A) private fun f()`) must still
        // reach the declaration — visibility is not silently lost.
        let mut d = DiagSink::new();
        let src = "class A\ncontext(a: A) private fun f() { }\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "unexpected: {}", d.render("t", src));
        let f = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Fun(f) if f.name == "f" => Some(f),
                _ => None,
            })
            .expect("fun f parsed");
        assert_eq!(f.context_count, 1);
        assert_eq!(f.visibility, Visibility::Private);
    }

    #[test]
    fn context_params_do_not_leak_to_later_function() {
        let mut d = DiagSink::new();
        let src = "class A\n\
                   context(a: A)\n\
                   val current: A get() = a\n\
                   fun g(): Int = 1\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        let property = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Property(property) if property.name == "current" => Some(property),
                _ => None,
            })
            .expect("property parsed");
        assert_eq!(property.context_params.len(), 1);
        assert_eq!(property.context_params[0].name, "a");
        assert_eq!(property.context_params[0].ty.name, "A");
        let g = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Fun(f) if f.name == "g" => Some(f),
                _ => None,
            })
            .expect("fun g parsed");
        assert_eq!(g.context_count, 0);
        assert_eq!(g.params.len(), 0);
    }

    #[test]
    fn visibility_modifiers_captured() {
        // The visibility modifier on a top-level declaration is captured onto its AST node (default
        // `public` when none is written) — the foundation the resolver's access checks read.
        let mut d = DiagSink::new();
        let src = "internal fun f() {}\nprivate fun g() {}\nfun p() {}\ninternal class C {}\nprivate val x = 1\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "unexpected: {}", d.render("t", src));
        let vis = |name: &str| -> Visibility {
            file.decls
                .iter()
                .find_map(|&id| match file.decl(id) {
                    Decl::Fun(f) if f.name == name => Some(f.visibility),
                    Decl::Class(c) if c.name == name => Some(c.visibility),
                    Decl::Property(pr) if pr.name == name => Some(pr.visibility),
                    _ => None,
                })
                .expect("declaration present")
        };
        assert_eq!(vis("f"), Visibility::Internal);
        assert_eq!(vis("g"), Visibility::Private);
        assert_eq!(vis("p"), Visibility::Public);
        assert_eq!(vis("C"), Visibility::Internal);
        assert_eq!(vis("x"), Visibility::Private);
    }

    #[test]
    fn nested_classifier_visibility_modifiers_captured() {
        let mut diagnostics = DiagSink::new();
        let source = "class Parent {\n\
                          private class Hidden\n\
                          protected class DerivedOnly\n\
                          internal class ModuleOnly\n\
                          class Visible\n\
                      }\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("test", source)
        );
        let visibility = |name: &str| {
            file.decls
                .iter()
                .find_map(|&declaration| match file.decl(declaration) {
                    Decl::Class(class) if class.name == name => Some(class.visibility),
                    _ => None,
                })
                .expect("nested classifier")
        };
        assert_eq!(visibility("Parent.Hidden"), Visibility::Private);
        assert_eq!(visibility("Parent.DerivedOnly"), Visibility::Protected);
        assert_eq!(visibility("Parent.ModuleOnly"), Visibility::Internal);
        assert_eq!(visibility("Parent.Visible"), Visibility::Public);
    }

    #[test]
    fn member_property_visibility_modifiers_captured() {
        // A class body's member PROPERTIES carry their own visibility. (Regular-class member FUNCTION
        // visibility is deliberately not fed to the AST yet: marking such a method `private` would emit
        // it non-virtually without the synthetic accessor krusty doesn't generate; see the parser
        // dispatch. Interface member functions and top-level functions do carry it.)
        let mut d = DiagSink::new();
        let src = "class C {\n  internal val mv = 1\n  private var pv = 2\n  val pub = 3\n}\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "unexpected: {}", d.render("t", src));
        let class = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Class(c) if c.name == "C" => Some(c),
                _ => None,
            })
            .expect("class C");
        let prop = |name: &str| {
            class
                .body_props
                .iter()
                .find(|p| p.name == name)
                .expect("property")
        };
        assert_eq!(prop("mv").visibility, Visibility::Internal);
        assert_eq!(prop("pv").visibility, Visibility::Private);
        assert_eq!(prop("pub").visibility, Visibility::Public);
    }

    #[test]
    fn simple_fun() {
        assert_eq!(
            tree("fun add(a: Int, b: Int): Int = a + b"),
            "(fun add (param a Int) (param b Int) :Int (+ a b))\n"
        );
    }

    /// Annotation capture (drives the compiler-extension surface): the parser records applied
    /// annotation source references on a class, attached to the RIGHT declaration, excluding use-site ones.
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
                    Decl::Class(c) if c.name == name => Some(
                        c.annotations
                            .iter()
                            .map(|annotation| annotation.name.clone())
                            .collect::<Vec<_>>(),
                    ),
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
        assert_eq!(
            q.iter()
                .map(|annotation| annotation.name.as_str())
                .collect::<Vec<_>>(),
            ["kotlinx.serialization.Serializable"]
        );
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
                    Decl::Fun(f) if f.name == name => Some(
                        f.annotations
                            .iter()
                            .map(|annotation| annotation.name.clone())
                            .collect::<Vec<_>>(),
                    ),
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
    fn property_annotation_does_not_leak_to_the_next_declaration() {
        let mut diagnostics = DiagSink::new();
        let source = "@Deprecated(\"old\")\nval oldProperty = 1\nclass Fresh\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("test.kt", source)
        );
        let class = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) if class.name == "Fresh" => Some(class),
                _ => None,
            })
            .expect("Fresh class");
        assert!(
            class.annotations.is_empty(),
            "property annotation leaked onto the following class"
        );
    }

    #[test]
    fn parses_annotated_explicit_primary_constructor_without_splitting_the_class() {
        let mut diagnostics = DiagSink::new();
        let source =
            "class Injected\n@Deprecated(\"old\") constructor(val value: Int)\nclass Fresh\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("test.kt", source)
        );
        assert_eq!(file.decls.len(), 2);
        let find_class = |name: &str| {
            file.decls
                .iter()
                .find_map(|&declaration| match file.decl(declaration) {
                    Decl::Class(class) if class.name == name => Some(class),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name} class"))
        };
        let class = find_class("Injected");
        assert_eq!(class.name, "Injected");
        assert_eq!(class.props.len(), 1);
        assert_eq!(class.props[0].name, "value");
        assert!(class.annotations.is_empty());
        assert!(find_class("Fresh").annotations.is_empty());
    }

    #[test]
    fn class_header_lookahead_keeps_sibling_annotation() {
        let mut diagnostics = DiagSink::new();
        let source = "class First\n@Deprecated(\"old\") class Second\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("test.kt", source)
        );
        assert_eq!(file.decls.len(), 2);
        let second = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) if class.name == "Second" => Some(class),
                _ => None,
            })
            .expect("Second class");
        assert_eq!(
            second
                .annotations
                .iter()
                .map(|annotation| annotation.name.as_str())
                .collect::<Vec<_>>(),
            ["Deprecated"]
        );
    }

    #[test]
    fn class_header_lookahead_stops_at_semicolon() {
        let mut diagnostics = DiagSink::new();
        let source =
            "class First; @Deprecated(\"old\") constructor(val value: Int)\nclass Second\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        let find_class = |name: &str| {
            file.decls
                .iter()
                .find_map(|&declaration| match file.decl(declaration) {
                    Decl::Class(class) if class.name == name => Some(class),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name} class"))
        };
        assert!(
            find_class("First").props.is_empty(),
            "constructor after semicolon was attached to First"
        );
        assert!(find_class("Second").annotations.is_empty());
    }

    #[test]
    fn every_declared_type_header_shares_newline_before_supertype_colon() {
        let mut diagnostics = DiagSink::new();
        let source = r#"
interface HeaderBase
class HeaderClass
    : HeaderBase
interface HeaderInterface
    : HeaderBase
object HeaderObject
    : HeaderBase
enum class HeaderEnum
    : HeaderBase { Entry }
class HeaderHost {
    companion object
        : HeaderBase
}
"#;
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected: {}",
            diagnostics.render("test.kt", source)
        );
        let find_class = |name: &str| {
            file.decls
                .iter()
                .find_map(|&declaration| match file.decl(declaration) {
                    Decl::Class(class) if class.name == name => Some(class),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name} declaration"))
        };

        // All declared-type entry points delegate to `parse_supertypes`; this assertion set guards
        // against a caller reintroducing its own same-line-only colon check (the enum parser did so
        // previously) and bypassing the common continuation rule.
        for name in [
            "HeaderClass",
            "HeaderInterface",
            "HeaderObject",
            "HeaderEnum",
        ] {
            assert_eq!(
                find_class(name)
                    .supertypes
                    .iter()
                    .map(|supertype| supertype.name.as_str())
                    .collect::<Vec<_>>(),
                ["HeaderBase"],
                "{name} did not retain the continued supertype header"
            );
        }
        let companion = match file.decl(find_class("HeaderHost").companion.expect("companion")) {
            Decl::Class(companion) => companion,
            _ => unreachable!("companion edge must target a class"),
        };
        assert!(companion.is_singleton());
        assert_eq!(
            companion
                .supertypes
                .iter()
                .map(|supertype| supertype.name.as_str())
                .collect::<Vec<_>>(),
            ["HeaderBase"]
        );
    }

    #[test]
    fn semicolon_does_not_continue_into_a_supertype_list() {
        let mut diagnostics = DiagSink::new();
        let source = "open class Base\nclass Derived; : Base()\n";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        let derived = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) if class.name == "Derived" => Some(class),
                _ => None,
            })
            .expect("Derived class");

        assert!(derived.supertypes.is_empty());
        assert!(diagnostics.diags.iter().any(|diagnostic| {
            diagnostic.msg == "expected a top-level declaration"
                && &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize] == ":"
        }));
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
            params[1]
                .annotations
                .iter()
                .map(|annotation| annotation.name.as_str())
                .collect::<Vec<_>>(),
            ["IntroducedAt"],
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
        assert_eq!(
            anns_of("content")
                .iter()
                .map(|annotation| annotation.name.as_str())
                .collect::<Vec<_>>(),
            ["Composable"]
        );
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
    fn prefix_unary_binds_more_tightly_than_cast() {
        assert_eq!(
            tree("fun f(): Int? = -10.toInt() as Int?"),
            "(fun f :Int (as (neg (call (. 10 toInt))) Int))\n"
        );
        assert_eq!(
            tree("fun g(x: Int, y: Int): Int = -x as Int * y"),
            "(fun g (param x Int) (param y Int) :Int (* (as (neg x) Int) y))\n"
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
    fn definitely_non_null_type_inside_explicit_call_arguments() {
        let mut diagnostics = DiagSink::new();
        let source = "fun <T> use(value: T) { exact<Box<T & Any>>(value) }";
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            diagnostics.diags.is_empty(),
            "{:?}",
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| &diagnostic.msg)
                .collect::<Vec<_>>()
        );
        let arguments = file
            .call_type_args
            .values()
            .next()
            .expect("explicit call type arguments");
        assert!(arguments[0].targs[0].definitely_non_null());
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
    fn generic_base_arguments_are_preserved_in_the_class_ast() {
        let source = "open class Base<T>\nclass Child : Base<Map<String, Int>>()";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(!diagnostics.has_errors());
        let child = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) if class.name == "Child" => Some(class),
                _ => None,
            })
            .unwrap();

        assert_eq!(child.base_class.as_deref(), Some("Base"));
        assert_eq!(child.base_type_args.len(), 1);
        assert_eq!(child.base_type_args[0].name, "Map");
        assert_eq!(
            child.base_type_args[0]
                .targs
                .iter()
                .map(|argument| argument.name.as_str())
                .collect::<Vec<_>>(),
            ["String", "Int"]
        );
    }

    #[test]
    fn where_constraint_must_name_a_declared_type_parameter() {
        let source = "class C<T> where U : Any";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let _ = parse(source, &tokens, &mut diagnostics);

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            ["'U' does not refer to a type parameter of 'C'."]
        );
    }

    #[test]
    fn constructor_property_modifiers_are_preserved_in_the_class_ast() {
        let source = "annotation class `final`\n\
                      open class Example(\n\
                      \u{20}\u{20}@`final`(open = true) override val first: String,\n\
                      \u{20}\u{20}final override val second: String,\n\
                      \u{20}\u{20}val third: String,\n\
                      )";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(!diagnostics.has_errors());
        let class = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) if class.name == "Example" => Some(class),
                _ => None,
            })
            .unwrap();

        assert!(class.props[0].is_override);
        assert!(class.props[0].is_open);
        assert!(class.props[1].is_override);
        assert!(!class.props[1].is_open);
        assert!(!class.props[2].is_override);
        assert!(!class.props[2].is_open);
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
        let until = tree("fun u(n: Int) {\n for (i in 0 until n step 2) {}\n}");
        assert!(
            until.contains("(call (. (call (. 0 until) n) step) 2)"),
            "{until}"
        );
        let open_end = tree("fun o(n: Int) {\n for (i in 0..<n step 2) {}\n}");
        assert!(
            open_end.contains("(call (. (..< 0 n) step) 2)"),
            "{open_end}"
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
        assert_eq!(
            tree("fun h(n: Int): Int = when (n) { 0, -> 1; else -> 2 }"),
            "(fun h (param n Int) :Int (when n (arm 0 => 1) (arm else => 2)))\n"
        );
        assert_eq!(
            tree(
                "fun i(n: Int): Int = when (n) {\n\
                     0,\n\
                     1,\n\
                         -> 1\n\
                     else -> 2\n\
                 }"
            ),
            "(fun i (param n Int) :Int (when n (arm 0 1 => 1) (arm else => 2)))\n"
        );

        let source = "fun invalid(): Int = when { true, -> 1; else -> 2 }";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        parse(source, &tokens, &mut diagnostics);
        assert!(
            diagnostics.has_errors(),
            "subjectless when accepted a comma-separated condition"
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
    fn enum_companion_is_an_ordinary_nested_singleton() {
        let mut d = DiagSink::new();
        let src = "enum class Game { ROCK, PAPER; companion object { fun foo() = ROCK; val bar = PAPER } }\n";
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        assert!(!d.has_errors(), "unexpected: {}", d.render("t", src));
        let g = file
            .decls
            .iter()
            .find_map(|&id| match file.decl(id) {
                Decl::Class(c) if c.name == "Game" => Some(c),
                _ => None,
            })
            .expect("enum Game parsed");
        assert_eq!(g.enum_entries.len(), 2);
        let companion = match file.decl(g.companion.expect("companion edge")) {
            Decl::Class(companion) => companion,
            _ => unreachable!("companion edge must target a class"),
        };
        assert_eq!(companion.name, "Game.Companion");
        assert!(companion.is_singleton());
        assert!(companion.methods.iter().any(|method| method.name == "foo"));
        assert!(companion
            .body_props
            .iter()
            .any(|property| property.name == "bar"));
    }

    #[test]
    fn multi_index_subscript_parses() {
        // `recv[i, j]` (two or more indices) parses as a multi-index `get` operator; a single index
        // stays the ordinary `Index`. The assignment form `recv[i, j] = v` is a multi-index `set`.
        assert_eq!(
            tree("fun f(m: M): Int = m[1, 2]"),
            "(fun f (param m M) :Int (index-multi m 1 2))\n"
        );
        assert_eq!(
            tree("fun f(m: M) { m[1, 2] = 3 }"),
            "(fun f (param m M) (block (set-index-multi m 1 2 3)))\n"
        );
        // A single index is unchanged.
        assert_eq!(
            tree("fun f(a: IntArray): Int = a[0]"),
            "(fun f (param a IntArray) :Int (index a 0))\n"
        );
    }

    #[test]
    fn prefix_member_value_expansion_reads_the_property_once() {
        // A prefix value must be the value produced by `inc()`, not a second read after the setter.
        // The debug tree exposes member READS as `(. probe value)`; the assignment target is a
        // distinct `set-member` node. Exactly one read therefore pins the single-evaluation
        // invariant without requiring the backend to support a custom-accessor fixture.
        let parsed =
            tree("fun f(probe: SyntheticProbe): Int = if (true) { ++probe.value } else { 0 }");
        assert_eq!(
            parsed.matches("(. probe value)").count(),
            1,
            "prefix expansion must read the member once: {parsed}"
        );
        assert!(
            parsed.contains("set-member"),
            "prefix expansion must retain the member assignment: {parsed}"
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

    #[test]
    fn multi_dollar_interpolation_honors_feature_overrides() {
        let source =
            "// LANGUAGE: -MultiDollarInterpolation\nfun render(value: String) = $$\"$$value\"";
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let features = LangFeatures::from_source(source);
        let _ = parse_with_features(source, &tokens, &mut diagnostics, &features);
        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            ["multi-dollar string interpolation is disabled by the language feature set"]
        );
    }
}
