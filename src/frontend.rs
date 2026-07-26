//! Frontend entry points.
//!
//! Source analysis: lexing, parsing, signature collection, and checking.

use crate::ast::File;
use crate::diag::{DiagSink, Severity};
use crate::features::LangFeatures;
pub use crate::lexer::{NameToken as FrontendNameToken, NameTokenKind as FrontendNameTokenKind};
use crate::libraries::{EmptySymbolSource, SemanticPlatform};
pub(crate) use crate::resolve::ClassSig as FrontendClassSig;
pub use crate::resolve::SymbolTable as FrontendSymbols;
pub use crate::resolve::TypeInfo as FrontendTypeInfo;
pub use crate::resolve::{
    check_file, check_file_at, check_file_in_source_set, collect_signatures,
    collect_signatures_with_cp, preinfer_module_returns, CompoundAssignmentTarget,
    SourceConstructorMatcher,
};
pub(crate) use crate::resolve::{
    pick_overload, qualified_path, typeref_leaf, ClassNames, CtorDefaultValue,
    DelegateGetValueTarget, DestructureComponentTarget, ExprLowering, InlineCall, InvokeKind,
    IteratorDispatchTarget, LambdaCapture, LambdaInfo, ReceiverLambda, ResolvedCall,
    ResolvedConstructor, ResolvedLocalFunctionCall, ResolvedMember, ResolvedModuleTopLevelCall,
    Signature, StmtLowering,
};
use crate::source::{SourceInput, SourceKind};

/// A single parsed file together with the frontend facts needed by a backend.
pub struct CheckedFile<'a> {
    pub file: &'a File,
    pub file_index: u32,
    pub info: &'a FrontendTypeInfo,
    pub symbols: &'a FrontendSymbols,
    /// The compilation's module name (kotlinc `-module-name`), for the serialization plugin's
    /// `write$Self$<module>` helper. `"main"` by default.
    pub module_name: &'a str,
}

/// Parsed and checked state for a jointly compiled source set.
pub struct SourceSetAnalysis {
    pub files: Vec<File>,
    pub symbols: FrontendSymbols,
    pub types: Vec<Option<FrontendTypeInfo>>,
}

/// Multiplatform `expect`/`actual` resolution over ONE compiled source set (kotlinc's JVM MPP
/// model: a platform module and its `dependsOn` chain compile as one set): drop every top-level
/// `expect` declaration for which some file supplies a matching non-`expect` counterpart — same
/// kind + name, and for callables the same arity and extension-receiver name. The `actual`
/// modifier itself is inert; an UNMATCHED `expect` stays in the tree and fails checking exactly
/// like any body-less declaration (skip, never mis-grade). Callers gate this on the
/// `MultiPlatformProjects` language feature, mirroring kotlinc.
/// The package-qualified expect/actual match key: `(package, kind, name, ext-receiver, arity)`.
type ExpectKey = (String, u8, String, String, usize);

pub fn strip_matched_expects(files: &mut [File]) {
    use crate::ast::Decl;
    // The match key is PACKAGE-qualified (expect/actual couple by FqName) but deliberately omits
    // the RETURN/property type and the receiver's TYPE ARGUMENTS (`List<String>.foo` keys as
    // `List`) — an `actual` routinely INFERS it (`actual fun greet() = "O"`), so a
    // type component would wrongly leave such pairs unmatched. kotlinc validates actual/expect
    // compatibility upstream; krusty trusts that and lets an incompatible pair fail checking on
    // its own terms downstream.
    fn key(file: &File, id: crate::ast::DeclId) -> ExpectKey {
        let pkg = file.package.clone().unwrap_or_default();
        let (kind, name, recv, arity) = match file.decl(id) {
            Decl::Fun(f) => (
                0,
                f.name.clone(),
                f.receiver
                    .as_ref()
                    .map(|r| r.name.clone())
                    .unwrap_or_default(),
                f.params.len(),
            ),
            Decl::Class(c) => (1, c.name.clone(), String::new(), 0),
            Decl::Property(p) => (
                2,
                p.name.clone(),
                p.receiver
                    .as_ref()
                    .map(|r| r.name.clone())
                    .unwrap_or_default(),
                0,
            ),
        };
        (pkg, kind, name, recv, arity)
    }
    // Pass 1: every NON-expect top-level declaration's key across the whole set. An
    // `actual typealias S = String` also actualizes an `expect class S` — typealiases live in
    // `File.type_aliases`, so add each alias NAME as a class-kind actual.
    let mut actuals: std::collections::HashSet<ExpectKey> = std::collections::HashSet::new();
    for file in files.iter() {
        for &d in &file.decls {
            if !file.expect_decls.contains(&d) {
                actuals.insert(key(file, d));
            }
        }
        for (alias, _) in &file.type_aliases {
            actuals.insert((
                file.package.clone().unwrap_or_default(),
                1,
                alias.clone(),
                String::new(),
                0,
            ));
        }
    }
    // Pass 1b: DEFAULT-ARGUMENT transplant. Parameter defaults live on the EXPECT declaration
    // (kotlinc forbids them on the actual), so dropping the expect would lose them and an
    // omitted-argument call site would mis-resolve. Harvest each matched expect fun's defaults as
    // COPYABLE expression trees; pass 2b grafts them onto the matching actual's parameters. A
    // default outside the copyable subset (literals/names/simple operators) is skipped — the
    // actual stays default-less there and an omitting call fails to resolve (skip, never wrong).
    // Alongside the defaults, the expect's PARAMETER NAMES: a default may reference a prior
    // parameter (`b: Int = a`), which only stays meaningful if the actual's names match — kotlinc
    // enforces exactly that (actual/expect parameter-name mismatch is an error), so a mismatch
    // here means invalid input; the graft is skipped rather than silently re-binding the name.
    type FunDefaults = (Vec<String>, Vec<Option<CopyExpr>>);
    fn harvest_fun(file: &File, f: &crate::ast::FunDecl) -> Option<FunDefaults> {
        if !f.params.iter().any(|p| p.default.is_some()) {
            return None;
        }
        let defs: Vec<Option<CopyExpr>> = f
            .params
            .iter()
            .map(|p| p.default.and_then(|e| CopyExpr::lift(file, e)))
            .collect();
        Some((f.params.iter().map(|p| p.name.clone()).collect(), defs))
    }
    fn graft_fun(
        f: &mut crate::ast::FunDecl,
        names: &[String],
        defs: &[Option<crate::ast::ExprId>],
    ) {
        let names_match = f
            .params
            .iter()
            .map(|p| p.name.as_str())
            .eq(names.iter().map(String::as_str));
        if !names_match {
            return; // invalid expect/actual pair (kotlinc rejects it) — don't graft
        }
        for (p, def) in f.params.iter_mut().zip(defs) {
            if p.default.is_none() {
                p.default = *def;
            }
        }
    }
    let mut expect_defaults: std::collections::HashMap<ExpectKey, FunDefaults> =
        std::collections::HashMap::new();
    // Member defaults of an `expect class`, keyed by the CLASS key + `(member name, arity)`.
    let mut expect_member_defaults: std::collections::HashMap<
        (ExpectKey, String, usize),
        FunDefaults,
    > = std::collections::HashMap::new();
    // Same-name same-arity member OVERLOADS (`f(Int = 1)` / `f(String = "b")`) collide on this
    // key; grafting either set of defaults onto both actuals would be type-wrong — poison the key
    // so neither is grafted (the omitting call then fails to resolve: skip, never wrong).
    let mut ambiguous_members: std::collections::HashSet<(ExpectKey, String, usize)> =
        std::collections::HashSet::new();
    for file in files.iter() {
        for &d in &file.expect_decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    if let Some(h) = harvest_fun(file, f) {
                        expect_defaults.insert(key(file, d), h);
                    }
                }
                Decl::Class(c) => {
                    for m in &c.methods {
                        let k = (key(file, d), m.name.clone(), m.params.len());
                        if ambiguous_members.contains(&k) {
                            continue;
                        }
                        if let Some(h) = harvest_fun(file, m) {
                            if expect_member_defaults.insert(k.clone(), h).is_some() {
                                expect_member_defaults.remove(&k);
                                ambiguous_members.insert(k);
                            }
                        } else if expect_member_defaults.remove(&k).is_some() {
                            // A defaulted overload next to a default-less same-key sibling is just
                            // as ambiguous for the graft target filter.
                            ambiguous_members.insert(k);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Pass 2: drop each matched expect declaration from its file's decl list; graft harvested
    // defaults onto the surviving actuals.
    for file in files.iter_mut() {
        let expects = std::mem::take(&mut file.expect_decls);
        let drop: Vec<crate::ast::DeclId> = expects
            .iter()
            .filter(|&&d| actuals.contains(&key(file, d)))
            .copied()
            .collect();
        file.decls.retain(|d| !drop.contains(d));
        file.expect_decls = expects.into_iter().filter(|d| !drop.contains(d)).collect();
        // Pass 2b: graft defaults. Two loops because materializing allocates into the file's
        // expr arena while the decl is temporarily detached.
        let decls: Vec<crate::ast::DeclId> = file.decls.clone();
        for d in decls {
            let k = key(file, d);
            if let Some((names, defs)) = expect_defaults.get(&k) {
                let materialized: Vec<Option<crate::ast::ExprId>> = defs
                    .iter()
                    .map(|c| c.as_ref().map(|c| c.materialize(file)))
                    .collect();
                let names = names.clone();
                if let Decl::Fun(f) = file.decl_mut(d) {
                    graft_fun(f, &names, &materialized);
                }
            }
            // Member defaults: an actual CLASS whose key matches an expect class with
            // defaulted members.
            let member_keys: Vec<(String, usize)> = expect_member_defaults
                .keys()
                .filter(|(ck, _, _)| *ck == k)
                .map(|(_, n, a)| (n.clone(), *a))
                .collect();
            for (mname, arity) in member_keys {
                let (names, materialized) = {
                    let (names, defs) = &expect_member_defaults[&(k.clone(), mname.clone(), arity)];
                    let m: Vec<Option<crate::ast::ExprId>> = defs
                        .iter()
                        .map(|c| c.as_ref().map(|c| c.materialize(file)))
                        .collect();
                    (names.clone(), m)
                };
                if let Decl::Class(c) = file.decl_mut(d) {
                    for m in c
                        .methods
                        .iter_mut()
                        .filter(|m| m.name == mname && m.params.len() == arity)
                    {
                        graft_fun(m, &names, &materialized);
                    }
                }
            }
        }
    }
}

/// A detached, owned copy of a SIMPLE expression tree (literals, names, unary/binary operators) —
/// enough for realistic parameter defaults — that can be re-materialized into another file's
/// arena. `lift` returns `None` for anything richer.
enum CopyExpr {
    Leaf(crate::ast::Expr),
    Binary {
        op: crate::ast::BinOp,
        lhs: Box<CopyExpr>,
        rhs: Box<CopyExpr>,
    },
}

impl CopyExpr {
    fn lift(file: &File, e: crate::ast::ExprId) -> Option<CopyExpr> {
        use crate::ast::Expr;
        Some(match file.expr(e) {
            leaf @ (Expr::IntLit(_)
            | Expr::LongLit(_)
            | Expr::UIntLit(_)
            | Expr::ULongLit(_)
            | Expr::DoubleLit(_)
            | Expr::FloatLit(_)
            | Expr::BoolLit(_)
            | Expr::StringLit(_)
            | Expr::CharLit(_)
            | Expr::NullLit
            | Expr::Name(_)) => CopyExpr::Leaf(leaf.clone()),
            Expr::Binary { op, lhs, rhs } => CopyExpr::Binary {
                op: *op,
                lhs: Box::new(CopyExpr::lift(file, *lhs)?),
                rhs: Box::new(CopyExpr::lift(file, *rhs)?),
            },
            _ => return None,
        })
    }

    fn materialize(&self, file: &mut File) -> crate::ast::ExprId {
        let span = crate::diag::Span::new(0, 0); // synthetic — diags never point at a grafted default
        match self {
            CopyExpr::Leaf(e) => file.add_expr(e.clone(), span),
            CopyExpr::Binary { op, lhs, rhs } => {
                let l = lhs.materialize(file);
                let r = rhs.materialize(file);
                file.add_expr(
                    crate::ast::Expr::Binary {
                        op: *op,
                        lhs: l,
                        rhs: r,
                    },
                    span,
                )
            }
        }
    }
}

/// Lex and parse one source string with an explicit feature set.
pub fn parse_source(src: &str, features: &LangFeatures, diags: &mut DiagSink) -> File {
    let tokens = crate::lexer::lex(src, diags);
    crate::parser::parse_with_features(src, &tokens, diags, features)
}

/// Tokenize only source names and the separators needed to interpret their declaration/reference
/// context. Process adapters use this frontend entry point rather than depending on lexer internals.
pub fn lex_name_tokens(src: &str, diags: &mut DiagSink) -> Vec<FrontendNameToken> {
    crate::lexer::lex_name_tokens(src, diags)
}

/// Lex and parse one source string after reading language-feature directives from the source.
pub fn parse_source_with_detected_features(src: &str, diags: &mut DiagSink) -> File {
    let features = LangFeatures::from_source(src);
    parse_source(src, &features, diags)
}

/// Analyze a source set with project-wide and per-source language features.
pub fn analyze_source_set_with_features(
    sources: &[SourceInput<'_>],
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> SourceSetAnalysis {
    analyze_source_set_with_features_and_prepare(
        sources,
        platform,
        project_features,
        |_, _| {},
        diags,
    )
}

pub fn analyze_source_set_with_features_and_prepare<F>(
    sources: &[SourceInput<'_>],
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
    prepare_symbols: F,
    diags: &mut DiagSink,
) -> SourceSetAnalysis
where
    F: FnOnce(&[File], &mut FrontendSymbols),
{
    let mut files = Vec::with_capacity(sources.len());
    let mut parse_errors = Vec::with_capacity(sources.len());
    let mut multiplatform = project_features.has("MultiPlatformProjects");
    for (index, source) in sources.iter().enumerate() {
        diags.set_file(index as u32);
        let mut features = project_features.clone();
        features.apply_source_directives(source.text);
        multiplatform |= features.has("MultiPlatformProjects");
        let diagnostics_before = diags.diags.len();
        let file = match source.kind {
            SourceKind::Kotlin | SourceKind::KotlinScript => {
                parse_source(source.text, &features, diags)
            }
        };
        parse_errors.push(
            diags.diags[diagnostics_before..]
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error),
        );
        files.push(file);
    }

    if multiplatform {
        strip_matched_expects(&mut files);
    }
    let mut symbols = collect_signatures_with_cp(&files, platform, diags);
    prepare_symbols(&files, &mut symbols);
    let types = check_source_set_skipping(&files, &mut symbols, &parse_errors, diags);
    SourceSetAnalysis {
        files,
        symbols,
        types,
    }
}

fn check_source_set_skipping(
    files: &[File],
    symbols: &mut FrontendSymbols,
    skip: &[bool],
    diags: &mut DiagSink,
) -> Vec<Option<FrontendTypeInfo>> {
    preinfer_module_returns(files, symbols, diags);
    files
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if skip.get(index).copied().unwrap_or(false) {
                None
            } else {
                diags.set_file(index as u32);
                Some(check_file_in_source_set(
                    files,
                    index as u32,
                    symbols,
                    diags,
                ))
            }
        })
        .collect()
}

/// Check a parsed source set whose signatures have already been collected.
pub fn check_source_set(
    files: &[File],
    symbols: &mut FrontendSymbols,
    diags: &mut DiagSink,
) -> Vec<Option<FrontendTypeInfo>> {
    check_source_set_skipping(files, symbols, &[], diags)
}

/// Analyze a source set using only per-source feature directives.
pub fn analyze_source_set(
    sources: &[&str],
    platform: Box<dyn SemanticPlatform>,
    diags: &mut DiagSink,
) -> SourceSetAnalysis {
    let inputs = sources
        .iter()
        .map(|source| SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    analyze_source_set_with_features(&inputs, platform, &LangFeatures::new(), diags)
}

/// Parse a single source and run signature collection plus checking against `platform`.
pub fn analyze_source(
    src: &str,
    platform: Box<dyn SemanticPlatform>,
    diags: &mut DiagSink,
) -> (File, Option<FrontendSymbols>, Option<FrontendTypeInfo>) {
    let mut files = vec![parse_source_with_detected_features(src, diags)];
    if diags.has_errors() {
        return (files.pop().unwrap_or_default(), None, None);
    }

    let mut syms = collect_signatures_with_cp(&files, platform, diags);
    if diags.has_errors() {
        return (files.pop().unwrap_or_default(), Some(syms), None);
    }

    let info = check_file(&files[0], &mut syms, diags);
    (files.pop().unwrap_or_default(), Some(syms), Some(info))
}

/// Parse and check a source with no external libraries.
pub fn analyze_source_standalone(
    src: &str,
    diags: &mut DiagSink,
) -> (File, Option<FrontendSymbols>, Option<FrontendTypeInfo>) {
    analyze_source(src, Box::new(EmptySymbolSource), diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{Diagnostic, Span};
    use crate::source::SourceInput;

    #[test]
    fn standalone_analysis_accepts_simple_function() {
        let mut diags = DiagSink::new();
        let (_file, syms, info) =
            analyze_source_standalone("fun box(): String = \"OK\"", &mut diags);
        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert!(syms.is_some());
        assert!(info.is_some());
    }

    #[test]
    fn standalone_analysis_reports_checker_errors() {
        let mut diags = DiagSink::new();
        let (_file, syms, info) = analyze_source_standalone("fun f(): Int = \"no\"", &mut diags);
        assert!(diags.has_errors());
        assert!(syms.is_some());
        assert!(info.is_some());
    }

    #[test]
    fn source_set_analysis_applies_multiplatform_actualization() {
        let source = "// LANGUAGE: +MultiPlatformProjects\n\
                      expect fun value(): String\n\
                      actual fun value(): String = \"OK\"\n\
                      fun box(): String = value()";
        let inputs = [SourceInput::kotlin(source)];
        let mut diags = DiagSink::new();
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diags,
        );
        assert!(!diags.has_errors(), "{:?}", diags.diags);
        assert!(analysis.types[0].is_some());
    }

    #[test]
    fn preexisting_warning_does_not_mark_a_source_as_unparseable() {
        let mut diags = DiagSink::new();
        diags.diags.push(Diagnostic {
            span: Span::new(0, 0),
            severity: Severity::Warning,
            msg: "existing warning".to_string(),
            file: 0,
        });
        let inputs = [SourceInput::kotlin("fun value(): Int = 1")];
        let analysis = analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diags,
        );
        assert!(analysis.types[0].is_some());
    }
}
