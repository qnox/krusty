//! Frontend entry points.
//!
//! Source analysis: lexing, parsing, signature collection, and checking.

use crate::ast::File;
use crate::diag::{DiagSink, Severity};
use crate::features::LangFeatures;
pub use crate::lexer::{NameToken as FrontendNameToken, NameTokenKind as FrontendNameTokenKind};
use crate::libraries::{EmptySymbolSource, SemanticPlatform};
pub(crate) use crate::resolve::class_internal_resolver;
pub(crate) use crate::resolve::ClassSig as FrontendClassSig;
pub(crate) use crate::resolve::DeclaredPropertySig as FrontendDeclaredPropertySig;
pub use crate::resolve::SymbolTable as FrontendSymbols;
pub use crate::resolve::TypeInfo as FrontendTypeInfo;
pub use crate::resolve::{
    check_file, check_file_at, check_file_in_source_set, collect_signatures,
    collect_signatures_with_cp, preinfer_module_returns, AnonymousObjectCapture,
    CompoundAssignmentTarget, SourceConstructorMatcher,
};
pub(crate) use crate::resolve::{
    classifier_over_default, function_import_scope, pick_overload, qualified_path, typeref_leaf,
    ClassNames, CtorDefaultValue, DelegateGetValueTarget, DestructureComponentTarget, ExprLowering,
    FunctionImportScope, InlineCall, InvokeKind, IteratorDispatchTarget, LambdaCapture, LambdaInfo,
    ReceiverFnValueOrigin, ReceiverLambda, ResolvedCall, ResolvedConstructor,
    ResolvedCtorDelegationTarget, ResolvedLocalFunctionCall, ResolvedMember,
    ResolvedModuleTopLevelCall, SigFlags, Signature, StmtLowering,
};
/// Types carried by the public source-set analysis signatures, re-exported here so process
/// adapters do not have to reach through the frontend boundary into source classification.
pub use crate::source::{SourceInput, SourceKind};

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
            Expr::Binary { op, lhs, rhs, .. } => CopyExpr::Binary {
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
                        operator_span: span,
                    },
                    span,
                )
            }
        }
    }
}

/// Lex and parse one source string with an explicit feature set.
pub fn parse_source(src: &str, features: &LangFeatures, diags: &mut DiagSink) -> File {
    parse_source_kind(src, SourceKind::Kotlin, features, diags)
}

fn parse_source_kind(
    src: &str,
    kind: SourceKind,
    features: &LangFeatures,
    diags: &mut DiagSink,
) -> File {
    if kind == SourceKind::Java {
        return File::default();
    }
    let tokens = crate::lexer::lex(src, diags);
    match kind {
        SourceKind::Kotlin => crate::parser::parse_with_features(src, &tokens, diags, features),
        SourceKind::KotlinScript => {
            crate::parser::parse_script_with_features(src, &tokens, diags, features)
        }
        SourceKind::Java => unreachable!(),
    }
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

/// Analyze a source set with checked, inferred, and declaration-only file prefixes.
pub fn analyze_source_set_prefix_with_features(
    sources: &[SourceInput<'_>],
    checked_count: usize,
    inferred_count: usize,
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> SourceSetAnalysis {
    analyze_source_set_with_features_and_prepare_prefix(
        sources,
        checked_count,
        inferred_count,
        platform,
        project_features,
        |_, _| {},
        diags,
    )
}

/// Analyze an LSP source set and release support-file bodies after their final use.
pub fn analyze_source_set_prefix_with_features_trimmed(
    sources: &[SourceInput<'_>],
    checked_count: usize,
    inferred_count: usize,
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> SourceSetAnalysis {
    analyze_source_set_impl(
        sources,
        checked_count,
        inferred_count,
        platform,
        project_features,
        |_, _| {},
        diags,
        true,
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
    analyze_source_set_with_features_and_prepare_prefix(
        sources,
        sources.len(),
        sources.len(),
        platform,
        project_features,
        prepare_symbols,
        diags,
    )
}

fn analyze_source_set_with_features_and_prepare_prefix<F>(
    sources: &[SourceInput<'_>],
    checked_count: usize,
    inferred_count: usize,
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
    prepare_symbols: F,
    diags: &mut DiagSink,
) -> SourceSetAnalysis
where
    F: FnOnce(&[File], &mut FrontendSymbols),
{
    analyze_source_set_impl(
        sources,
        checked_count,
        inferred_count,
        platform,
        project_features,
        prepare_symbols,
        diags,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn analyze_source_set_impl<F>(
    sources: &[SourceInput<'_>],
    checked_count: usize,
    inferred_count: usize,
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
    prepare_symbols: F,
    diags: &mut DiagSink,
    trim_support_bodies: bool,
) -> SourceSetAnalysis
where
    F: FnOnce(&[File], &mut FrontendSymbols),
{
    let diagnostics_start = diags.diags.len();
    let mut files = Vec::with_capacity(sources.len());
    let mut parse_errors = Vec::with_capacity(sources.len());
    let mut multiplatform = project_features.has("MultiPlatformProjects");
    for (index, source) in sources.iter().enumerate() {
        diags.set_file(index as u32);
        let mut features = project_features.clone();
        features.apply_source_directives(source.text);
        multiplatform |= features.has("MultiPlatformProjects");
        let diagnostics_before = diags.diags.len();
        let file = parse_source_kind(source.text, source.kind, &features, diags);
        parse_errors.push(
            source.kind == SourceKind::Java
                || diags.diags[diagnostics_before..]
                    .iter()
                    .any(|diagnostic| diagnostic.severity == Severity::Error),
        );
        files.push(file);
    }

    assert!(checked_count <= inferred_count && inferred_count <= files.len());
    if multiplatform {
        strip_matched_expects(&mut files);
    }
    let platform = if inferred_count < files.len() {
        let mut fallback_diags = DiagSink::new();
        let mut fallback =
            collect_signatures_with_cp(&files[inferred_count..], platform, &mut fallback_diags);
        fallback.offset_source_files(inferred_count as u32);
        let platform = std::mem::replace(&mut fallback.libraries, Box::new(EmptySymbolSource));
        Box::new(crate::resolve::SourceFallbackPlatform::new(
            platform, fallback,
        )) as Box<dyn SemanticPlatform>
    } else {
        platform
    };
    let inferred_end = inferred_count.min(files.len());
    if trim_support_bodies {
        for file in &mut files[inferred_end..] {
            file.release_body_arenas();
        }
    }
    let mut symbols = collect_signatures_with_cp(&files[..inferred_end], platform, diags);
    prepare_symbols(&files, &mut symbols);
    preinfer_module_returns(&files[..inferred_end], &mut symbols, diags);
    if trim_support_bodies {
        for file in &mut files[checked_count.min(inferred_end)..inferred_end] {
            file.release_body_arenas();
        }
    }
    let types =
        check_source_set_skipping(&files, &mut symbols, &parse_errors, checked_count, diags);
    diags.collapse_duplicates_from(diagnostics_start);
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
    checked_count: usize,
    diags: &mut DiagSink,
) -> Vec<Option<FrontendTypeInfo>> {
    files
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index >= checked_count || skip.get(index).copied().unwrap_or(false) {
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
    let diagnostics_start = diags.diags.len();
    preinfer_module_returns(files, symbols, diags);
    let types = check_source_set_skipping(files, symbols, &[], files.len(), diags);
    diags.collapse_duplicates_from(diagnostics_start);
    types
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
    use crate::libraries::{
        CallSig, Callables, FnKind, FunctionInfo, FunctionSet, GenericSig, LibraryCallable,
        LibraryMember, LibraryType, PropKind, PropertyInfo, PropertySet, ResolvedSymbols, TypeKind,
    };
    use crate::source::SourceInput;
    use crate::types::{Ty, TypeNameList, Visibility};

    struct ExistingLibrary;

    impl crate::symbol_source::SymbolSource for ExistingLibrary {
        fn resolve_type(&self, internal: &str) -> Option<LibraryType> {
            matches!(
                internal,
                "fixture/Present"
                    | "fixture/Stable"
                    | "fixture/Qualified"
                    | "fixture/Container"
                    | "fixture/Container$Labels"
                    | "support/BaseScope"
                    | "support/BaseTarget"
                    | "support/Target"
                    | "fixture/Outer"
                    | "fixture/Outer$Hidden"
                    | "fixture/Outer$Hidden$Context"
                    | "fixture/CollisionEnum"
            )
            .then(|| {
                let mut supertypes = TypeNameList::new();
                if internal == "support/Target" {
                    supertypes.push("support/BaseTarget");
                }
                LibraryType {
                    is_public: true,
                    kind: if internal == "fixture/Container$Labels" {
                        TypeKind::Object
                    } else {
                        TypeKind::Class
                    },
                    supertypes,
                    constructors: Vec::new(),
                    fields: Vec::new(),
                    members: Vec::new(),
                    companion: match internal {
                        "fixture/Stable" => vec![LibraryMember::new(
                            "current".to_string(),
                            Vec::new(),
                            Ty::Int,
                            String::new(),
                        )],
                        "fixture/Qualified" => vec![LibraryMember::new(
                            "select".to_string(),
                            vec![Ty::obj("right/Token")],
                            Ty::Int,
                            String::new(),
                        )],
                        _ => Vec::new(),
                    },
                    companion_consts: std::collections::HashMap::new(),
                    sam_method: None,
                    companion_object: None,
                    value_companion_fns: Vec::new(),
                    value_underlying: None,
                    alias_target: None,
                    type_params: Vec::new(),
                    sealed_subclasses: TypeNameList::new(),
                    enum_entries: Vec::new(),
                    enum_entries_accessor: None,
                    value_ctor_has_default: false,
                    ctor_named_params: Vec::new(),
                    value_class_properties: Vec::new(),
                    retention: None,
                }
            })
        }

        fn resolve_symbols(&self, fqn: &str) -> ResolvedSymbols {
            let classifier = self.resolve_type(fqn).map(std::rc::Rc::new);
            let Some(name) = fqn
                .strip_prefix("support/")
                .filter(|name| matches!(*name, "adjust" | "configure" | "transform"))
            else {
                return ResolvedSymbols {
                    classifier,
                    ..ResolvedSymbols::default()
                };
            };
            let receiver = Ty::obj("support/Target");
            let lambda_receiver = Ty::obj("support/BaseScope");
            let mut value_params = Vec::new();
            if name == "adjust" {
                value_params.push(Ty::Int);
            }
            value_params.push(Ty::fun(vec![lambda_receiver], Ty::Unit));
            let mut physical_params = vec![receiver];
            physical_params.extend(value_params.iter().copied());
            let callable = LibraryCallable::library(
                "support/SupportKt",
                name,
                physical_params,
                Ty::Unit,
                Ty::Unit,
                "",
            );
            let mut function = FunctionInfo::plain(FnKind::Extension, Some(receiver), callable);
            let mut lambda_param_types = vec![Vec::new(); value_params.len()];
            *lambda_param_types.last_mut().unwrap() = vec![lambda_receiver];
            function.call_sig = CallSig {
                lambda_param_types,
                lambda_receivers: vec![None; value_params.len()],
                required: value_params.len(),
                ..CallSig::default()
            };
            function.generic_sig = Some(GenericSig {
                formals: Vec::new(),
                formal_bounds: Vec::new(),
                receiver: Some(receiver),
                params: value_params,
                ret: Ty::Unit,
            });
            ResolvedSymbols {
                classifier,
                callables: Callables::Functions(FunctionSet {
                    overloads: vec![function],
                }),
            }
        }

        fn property_members(&self, recv: Ty, name: &str) -> PropertySet {
            if recv == Ty::obj("fixture/Container$Labels") && name == "marker" {
                PropertySet {
                    overloads: vec![PropertyInfo {
                        kind: PropKind::Member,
                        receiver: Some(recv),
                        formals: Vec::new(),
                        ty: Ty::Int,
                        context_count: 0,
                        getter: LibraryCallable::library(
                            "fixture/Container$Labels",
                            "getMarker",
                            Vec::new(),
                            Ty::Int,
                            Ty::Int,
                            "()I",
                        ),
                        setter: None,
                        is_const: false,
                        visibility: Visibility::Private,
                        owner: "fixture/Container$Labels".into(),
                        receiver_rank: 0,
                        source_key: None,
                    }],
                }
            } else {
                PropertySet::default()
            }
        }
    }

    impl SemanticPlatform for ExistingLibrary {
        fn static_field(
            &self,
            internal: &str,
            name: &str,
        ) -> Option<crate::libraries::StaticFieldRef> {
            (internal == "fixture/CollisionEnum" && name == "ANY").then(|| {
                crate::libraries::StaticFieldRef {
                    owner: crate::types::type_name(internal),
                    name: name.to_string(),
                    descriptor: "Lfixture/CollisionEnum;".to_string(),
                    ty: Ty::obj("fixture/CollisionEnum"),
                    constant: None,
                }
            })
        }

        fn static_field_name(
            &self,
            internal: crate::types::TypeName,
            name: &str,
        ) -> Option<crate::libraries::StaticFieldRef> {
            self.static_field(&internal.render(), name)
        }
    }

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
    fn checked_prefix_reports_cross_file_conflicting_overloads_and_candidates() {
        let target = "fun namedPair(left: Int, right: String): Int = left\n\
                      fun missingNamedArgument(): Int = namedPair(left = 1)";
        let inputs = [
            SourceInput::kotlin(target),
            SourceInput::kotlin("fun namedPair(left: Int, right: String): String = right"),
            SourceInput::kotlin("fun namedPair(left: Int, right: String): String = right"),
        ];
        let mut diagnostics = DiagSink::new();

        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(analysis.types[0].is_some());
        assert_eq!(
            diagnostics
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.file == 0)
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            [
                "conflicting overloads:\n\
                 fun namedPair(left: Int, right: String): String\n\
                 fun namedPair(left: Int, right: String): String",
                "no value passed for parameter 'right'.",
                "none of the following candidates is applicable:\n\n\
                 fun namedPair(left: Int, right: String): Int\n\
                 fun namedPair(left: Int, right: String): String\n\
                 fun namedPair(left: Int, right: String): String",
            ]
        );
        let target_diagnostics = diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 0)
            .collect::<Vec<_>>();
        assert_eq!(
            &target[target_diagnostics[0].span.lo as usize..target_diagnostics[0].span.hi as usize],
            "fun namedPair(left: Int, right: String): Int"
        );
        for diagnostic in &target_diagnostics[1..] {
            let editor_span = diagnostic.editor_span.unwrap_or(diagnostic.span);
            assert_eq!(
                &target[editor_span.lo as usize..editor_span.hi as usize],
                "namedPair"
            );
        }
    }

    #[test]
    fn conflicting_top_level_bodies_use_their_own_declared_return_types() {
        let inputs = [
            SourceInput::kotlin("fun choose(value: Int): Int = value"),
            SourceInput::kotlin("fun choose(value: Int): String = \"ok\""),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            inputs.len(),
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            [
                "conflicting overloads:\nfun choose(value: Int): String",
                "conflicting overloads:\nfun choose(value: Int): Int",
            ]
        );
    }

    #[test]
    fn jvm_name_decides_the_top_level_clash() {
        // A platform declaration clash is keyed on the EMITTED name. `g(String)` and `g(String?)`
        // erase to the same JVM descriptor, so they clash while both are spelled `g`…
        let clash = |sources: &[&str]| {
            let inputs = sources
                .iter()
                .map(|source| SourceInput::kotlin(source))
                .collect::<Vec<_>>();
            let mut diagnostics = DiagSink::new();
            analyze_source_set_prefix_with_features(
                &inputs,
                inputs.len(),
                inputs.len(),
                Box::new(EmptySymbolSource),
                &LangFeatures::new(),
                &mut diagnostics,
            );
            diagnostics
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.msg.starts_with("conflicting overloads:"))
                .count()
        };
        assert_eq!(
            clash(&["fun g(x: String): String = \"nn\"\nfun g(x: String?): String = \"nl\""]),
            2,
        );
        // …and stop clashing once `@JvmName` gives one of them a different bytecode name.
        assert_eq!(
            clash(&["fun g(x: String): String = \"nn\"\n\
                 @JvmName(\"gNullable\")\n\
                 fun g(x: String?): String = \"nl\""]),
            0,
        );
        // The same key catches the reverse: distinct SOURCE names collapsed onto one JVM name do
        // clash, which the source-name key could never see.
        assert_eq!(
            clash(&["@JvmName(\"same\")\n\
                 fun a(x: Int): String = \"a\"\n\
                 @JvmName(\"same\")\n\
                 fun b(x: Int): String = \"b\""]),
            2,
        );
    }

    #[test]
    fn inferred_conflict_displays_only_source_signature_types() {
        let inputs = [
            SourceInput::kotlin("package sample\nclass Result\nfun choose(value: Int) = Result()"),
            SourceInput::kotlin("package sample\nfun choose(value: Int) = Result()"),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            inputs.len(),
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            [
                "conflicting overloads:\nfun choose(value: Int)",
                "conflicting overloads:\nfun choose(value: Int)",
            ]
        );
    }

    #[test]
    fn mixed_private_public_conflicts_retain_visible_representatives_in_either_order() {
        for public_first in [false, true] {
            let private_declarations = (0..64)
                .map(|index| {
                    format!(
                        "private fun crowded(value: Int, required: String): Int = value // {index}\n"
                    )
                })
                .collect::<String>();
            let public_declarations = (0..64)
                .map(|index| {
                    format!(
                        "fun crowded(value: Int, required: String): String = required // {index}\n"
                    )
                })
                .collect::<String>();
            let source = if public_first {
                format!(
                    "{public_declarations}{private_declarations}\
                     fun use(): Int = crowded(value = 1)"
                )
            } else {
                format!(
                    "{private_declarations}{public_declarations}\
                     fun use(): Int = crowded(value = 1)"
                )
            };
            let inputs = [SourceInput::kotlin(&source)];
            let mut diagnostics = DiagSink::new();

            analyze_source_set_prefix_with_features(
                &inputs,
                inputs.len(),
                inputs.len(),
                Box::new(EmptySymbolSource),
                &LangFeatures::new(),
                &mut diagnostics,
            );

            let candidate_report = diagnostics
                .diags
                .iter()
                .find(|diagnostic| {
                    diagnostic
                        .msg
                        .starts_with("none of the following candidates is applicable:")
                })
                .expect("conflicting call should report its retained candidates");
            assert!(
                candidate_report
                    .msg
                    .contains("fun crowded(value: Int, required: String): String"),
                "public declaration must survive private candidates when public_first={public_first}"
            );
            assert!(
                candidate_report
                    .msg
                    .contains("private fun crowded(value: Int, required: String): Int")
                    || candidate_report
                        .msg
                        .contains("fun crowded(value: Int, required: String): Int"),
                "private declaration must survive public candidates when public_first={public_first}"
            );
            assert!(candidate_report.msg.lines().skip(2).count() <= 64);
        }
    }

    #[test]
    fn conflicting_overload_diagnostics_sort_candidate_displays_stably() {
        let inputs = [
            SourceInput::kotlin(
                "fun namedPair(left: Int, right: String): String = right\n\
                 fun use(): String = namedPair(left = 1, unknown = 2, right = \"ok\")",
            ),
            SourceInput::kotlin("fun namedPair(left: Int, right: String): String = right"),
            SourceInput::kotlin("fun namedPair(left: Int, right: String): Int = left"),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.file == 0)
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            [
                "conflicting overloads:\n\
                 fun namedPair(left: Int, right: String): Int\n\
                 fun namedPair(left: Int, right: String): String",
                "no parameter with name 'unknown' found.",
                "none of the following candidates is applicable:\n\n\
                 fun namedPair(left: Int, right: String): Int\n\
                 fun namedPair(left: Int, right: String): String\n\
                 fun namedPair(left: Int, right: String): String",
            ]
        );
    }

    #[test]
    fn conflict_recovery_uses_alias_and_qualified_scopes() {
        let target = "package use\n\
                      import a.pick as choose\n\
                      fun aliasUse(): Int = choose(value = 1)\n\
                      fun qualifiedUse(): Int = a.pick(value = 1)";
        let inputs = [
            SourceInput::kotlin(target),
            SourceInput::kotlin("package a\nfun pick(value: Int, other: String): Int = value"),
            SourceInput::kotlin("package a\nfun pick(value: Int, other: String): String = other"),
            SourceInput::kotlin("package b\nfun pick(value: Int, other: String): Boolean = true"),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let messages = diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 0)
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.starts_with("no value passed"))
                .count(),
            2
        );
        let candidates = messages
            .iter()
            .filter(|message| {
                message.starts_with("none of the following candidates is applicable:")
            })
            .collect::<Vec<_>>();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|message| {
            message.contains("fun pick(value: Int, other: String): Int")
                && message.contains("fun pick(value: Int, other: String): String")
                && !message.contains("Boolean")
        }));
    }

    #[test]
    fn conflicting_overload_diagnostics_are_deterministic_and_bounded() {
        let sources = (0..70)
            .map(|index| format!("fun crowded(value: Int): Int = value // {index}"))
            .collect::<Vec<_>>();
        let inputs = sources
            .iter()
            .map(|source| SourceInput::kotlin(source))
            .collect::<Vec<_>>();
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let conflicts = diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.msg.starts_with("conflicting overloads:"))
            .collect::<Vec<_>>();
        assert_eq!(conflicts.len(), 70);
        assert_eq!(
            conflicts
                .iter()
                .map(|diagnostic| diagnostic.file)
                .collect::<Vec<_>>(),
            (0..70).collect::<Vec<_>>()
        );
        assert_eq!(conflicts[0].msg.lines().skip(1).count(), 64);
        assert!(conflicts
            .iter()
            .all(|diagnostic| diagnostic.msg.len() <= 64 * 1024));
        assert!(
            conflicts
                .iter()
                .map(|diagnostic| diagnostic.msg.len())
                .sum::<usize>()
                <= 4 * 1024 * 1024
        );
    }

    #[test]
    fn exhausted_conflict_display_budget_preserves_qualified_call_fallback() {
        let parameter = "p".repeat(70 * 1024);
        let declarations = [
            format!("package sample\nfun crowded({parameter}: Int): Int = {parameter}"),
            format!("package sample\nfun crowded({parameter}: Int): String = \"value\""),
        ];
        let inputs = [
            SourceInput::kotlin("package use\nfun use(): Int = sample.crowded(unknown = 1)"),
            SourceInput::kotlin(&declarations[0]),
            SourceInput::kotlin(&declarations[1]),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        let target_messages = diagnostics
            .diags
            .iter()
            .filter(|diagnostic| diagnostic.file == 0)
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>();
        assert!(target_messages.contains(&"no parameter with name 'unknown' found."));
        assert!(!target_messages.contains(&"none of the following candidates is applicable:"));
        assert!(!target_messages
            .iter()
            .any(|message| message.starts_with("unresolved reference")));
    }

    #[test]
    fn unrelated_inferred_return_arity_diagnostic_keeps_return_type() {
        let inputs = [SourceInput::kotlin(
            "fun inferred() = 1\nfun use(): Int = inferred(1)",
        )];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            inputs.len(),
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(diagnostics.diags.iter().any(|diagnostic| {
            diagnostic.msg == "too many arguments for 'fun inferred(): Int'."
        }));
    }

    #[test]
    fn cross_file_private_top_level_functions_do_not_conflict_or_escape_scope() {
        let target = "fun namedPair(left: Int, right: String): Int = left\n\
                      fun missingNamedArgument(): Int = namedPair(left = 1)";
        let inputs = [
            SourceInput::kotlin(target),
            SourceInput::kotlin("private fun namedPair(left: Int, right: String): String = right"),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.file == 0)
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            ["no value passed for parameter 'right'."]
        );
        assert!(
            diagnostics
                .diags
                .iter()
                .all(|diagnostic| !diagnostic.msg.starts_with("conflicting overloads:")),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn cross_file_private_top_level_callable_reference_reports_visibility() {
        let inputs = [
            SourceInput::kotlin("val reference: (Int) -> Int = ::hidden"),
            SourceInput::kotlin("private fun hidden(value: Int): Int = value"),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            inputs.len(),
            inputs.len(),
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .filter(|diagnostic| diagnostic.file == 0)
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            ["cannot access 'hidden': it is private in its file"]
        );
    }

    #[test]
    fn unavailable_context_does_not_hide_inapplicable_candidate_family() {
        let source = "class Scope\n\
                      context(scope: Scope) fun choose(value: Int): Int = value\n\
                      context(scope: Scope) fun choose(other: Int): String = \"\"\n\
                      fun use(): Int = choose(value = 1)";
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &[SourceInput::kotlin(source)],
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(
            diagnostics.diags.iter().any(|diagnostic| {
                diagnostic.msg
                    == "none of the following candidates is applicable:\n\n\
                        context(scope: Scope) fun choose(other: Int): String\n\
                        context(scope: Scope) fun choose(value: Int): Int"
            }),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn script_analysis_respects_declaration_order() {
        let mut diags = DiagSink::new();
        let inputs = [SourceInput::new(
            SourceKind::KotlinScript,
            "fun read(): Int = value\nval value = 1",
        )];
        analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diags,
        );
        assert!(diags.has_errors());

        let mut diags = DiagSink::new();
        let inputs = [SourceInput::new(
            SourceKind::KotlinScript,
            "val value = 1\nfun read(): Int = value\nread()",
        )];
        analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diags,
        );
        assert!(!diags.has_errors(), "{:?}", diags.diags);
    }

    #[test]
    fn script_declarations_do_not_enter_module_scope() {
        let mut diags = DiagSink::new();
        let inputs = [
            SourceInput::new(
                SourceKind::KotlinScript,
                "fun scriptFunction(): Int = 1\n\
                 class ScriptClass\n\
                 ScriptClass()\n\
                 scriptFunction()",
            ),
            SourceInput::kotlin(
                "fun useFunction(): Int = scriptFunction()\n\
                 fun useClass(): ScriptClass = ScriptClass()",
            ),
            SourceInput::new(
                SourceKind::KotlinScript,
                "class ScriptClass\nval instance = ScriptClass()",
            ),
        ];
        analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diags,
        );

        assert!(diags.diags.iter().any(|diagnostic| diagnostic.file == 1));
        assert!(!diags.diags.iter().any(|diagnostic| diagnostic.file == 0));
        assert!(!diags.diags.iter().any(|diagnostic| diagnostic.file == 2));
    }

    #[test]
    fn script_analysis_rejects_jumps_without_an_enclosing_target() {
        let mut diags = DiagSink::new();
        let inputs = [SourceInput::new(
            SourceKind::KotlinScript,
            "return\nbreak\ncontinue",
        )];
        analyze_source_set_with_features(
            &inputs,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diags,
        );

        assert_eq!(diags.diags.len(), 3);
    }

    #[test]
    fn dependency_fallback_preserves_primary_and_adds_missing_signatures() {
        let features = LangFeatures::new();
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_prefix_with_features(
            &[
                SourceInput::kotlin(
                    "package feature\n\
                     import fixture.Qualified\n\
                     import fixture.Stable\n\
                     import fixture.added\n\
                     import left.Token\n\
                     fun use(): Int = Stable.current() + Stable.current(1) + Qualified.select(Token()) + added(1)",
                ),
                SourceInput::kotlin(
                    "package fixture\nimport left.Token\n\
                     fun added(value: Int): Int = value\n\
                     class Present\n\
                     class Stable { companion object {\n\
                     \u{20} fun current(): String = \"source\"\n\
                     \u{20} fun current(value: Int): Int = value\n\
                     } }\n\
                     class Qualified { companion object {\n\
                     \u{20} fun select(value: Token): Int = 1\n\
                     } }\n\
                     class Added",
                ),
                SourceInput::kotlin("package left\nclass Token"),
            ],
            1,
            1,
            Box::new(ExistingLibrary),
            &features,
            &mut diagnostics,
        );
        assert!(
            analysis.types[0].is_some() && diagnostics.diags.is_empty(),
            "{:?}",
            diagnostics.diags
        );
        let added = analysis.symbols.libraries.resolve_symbols("fixture/added");
        let Callables::Functions(functions) = added.callables else {
            panic!("missing source fallback function")
        };
        assert_eq!(functions.overloads[0].source_key.map(|key| key.0), Some(1));
    }

    #[test]
    fn dependency_receiver_shape_beats_stale_library_shape() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import support.BaseScope\n\
                 import support.Target\n\
                 import support.adjust\n\
                 import support.configure\n\
                 import support.transform\n\
                 object Owner {\n\
                     fun create(target: Target) = target.configure { assign() }\n\
                     fun update(target: Target) = target.transform { scope -> scope.assign() }\n\
                     fun change(target: Target) = target.adjust(1) { scope -> scope.assign() }\n\
                     private fun BaseScope.assign() {}\n\
                 }",
            ),
            SourceInput::kotlin(
                "package support\n\
                 open class BaseTarget\n\
                 class Target : BaseTarget()\n\
                 open class BaseScope\n\
                 inline fun Target.configure(block: BaseScope.() -> Unit) {}\n\
                 inline fun BaseTarget.transform(block: BaseScope.() -> Unit) {}\n\
                 inline fun Target.adjust(value: String, block: BaseScope.() -> Unit) {}",
            ),
        ];
        let mut diagnostics = DiagSink::new();

        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(ExistingLibrary),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(
            analysis.types[0].is_some() && diagnostics.diags.is_empty(),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn declaration_only_sources_hide_internal_classifiers() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import dependency.Hidden\n\
                 import dependency.Visible\n\
                 fun hidden(): Any = Hidden()\n\
                 fun visible(): Any = Visible()",
            ),
            SourceInput::kotlin(
                "package dependency\n\
                 internal class Hidden\n\
                 class Visible",
            ),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(diagnostics
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("'Hidden'")));
        assert!(!diagnostics
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("'Visible'")));
    }

    #[test]
    fn declaration_only_extension_calls_resolve_and_type() {
        // An imported extension from a DECLARATION-ONLY dependency file (beyond the inferred
        // prefix): its `Signature` lives in the fallback table behind the platform seam, not in
        // the checked prefix's symbol table, and the call must still resolve — including an
        // omitted defaulted parameter — and type as the declared return, not `Unit`.
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import dependency.render\n\
                 import dependency.tag\n\
                 class C {\n\
                 \u{20} fun go(): Int {\n\
                 \u{20}\u{20} val r = build()\n\
                 \u{20}\u{20} if (r == null) { return 0 }\n\
                 \u{20}\u{20} return r.length\n\
                 \u{20} }\n\
                 \u{20} fun build() = \"x\".tag()?.render()\n\
                 }",
            ),
            SourceInput::kotlin(
                "package dependency\n\
                 fun String?.tag(): String? = this\n\
                 fun String.render(prefix: (String) -> String = { it }): String = prefix(this)",
            ),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(
            diagnostics.diags.iter().all(|d| d.file != 0),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn modifier_prefixed_local_functions_parse_in_bodies() {
        // `tailrec fun`/`suspend fun` LOCAL declarations are statements in any body, not just
        // scripts — the soft-keyword prefix must not parse as an expression name.
        let inputs = [SourceInput::kotlin(
            "fun outer(n: Int): Int {\n\
             \u{20} tailrec fun down(k: Int): Int = if (k <= 0) 0 else down(k - 1)\n\
             \u{20} return down(n)\n\
             }",
        )];
        let mut diagnostics = DiagSink::new();
        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    }

    #[test]
    fn local_suspend_functions_are_rejected_cleanly() {
        // A local `suspend fun` has no CPS lowering for `Stmt::LocalFun` — one clear
        // diagnostic, never a mis-parsed soft keyword or a backend ICE.
        let inputs = [SourceInput::kotlin(
            "fun outer() {\n\
             \u{20} suspend fun inner() {}\n\
             \u{20} println(\"x\")\n\
             }",
        )];
        let mut diagnostics = DiagSink::new();
        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        assert!(
            diagnostics.diags.iter().any(|d| d
                .msg
                .contains("local 'suspend' functions are not supported")),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn declaration_only_source_exposes_qualified_nested_enum_entry() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import dependency.Model\n\
                 val context: Model.Context = Model.Context.ANY",
            ),
            SourceInput::kotlin(
                "package dependency\n\
                 class Model { enum class Context { ANY } }",
            ),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    }

    #[test]
    fn declaration_only_source_hides_public_nested_enum_of_internal_class() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import dependency.Hidden.Context\n\
                 val context: Any = Context.ANY",
            ),
            SourceInput::kotlin(
                "package dependency\n\
                 internal class Hidden { enum class Context { ANY } }",
            ),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(diagnostics
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("unresolved reference 'Context'")));
    }

    #[test]
    fn declaration_only_source_hides_public_enum_below_internal_nested_class() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import dependency.Outer\n\
                 val context: Outer.Hidden.Context? = null",
            ),
            SourceInput::kotlin(
                "package dependency\n\
                 class Outer { internal class Hidden { enum class Context { ANY } } }",
            ),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(diagnostics.diags.iter().any(|diagnostic| {
            diagnostic
                .msg
                .contains("unresolved reference 'Outer.Hidden.Context'")
        }));
    }

    #[test]
    fn declaration_only_internal_class_shadows_public_platform_type() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import fixture.*\n\
                 val hidden: Present? = null",
            ),
            SourceInput::kotlin("package fixture\ninternal class Present"),
        ];
        let mut diagnostics = DiagSink::new();

        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(ExistingLibrary),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(diagnostics
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("unresolved reference 'Present'")));
        assert!(analysis
            .symbols
            .libraries
            .resolve_symbols_name(crate::types::type_name("fixture/Present"))
            .classifier
            .is_none());
    }

    #[test]
    fn declaration_only_internal_nested_class_shadows_public_platform_path() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import fixture.Outer\n\
                 val hidden: Outer.Hidden.Context? = null",
            ),
            SourceInput::kotlin(
                "package fixture\n\
                 class Outer { internal class Hidden { class Context } }",
            ),
        ];
        let mut diagnostics = DiagSink::new();

        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(ExistingLibrary),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(diagnostics.diags.iter().any(|diagnostic| {
            diagnostic
                .msg
                .contains("unresolved reference 'Outer.Hidden.Context'")
        }));
        assert!(analysis
            .symbols
            .libraries
            .resolve_symbols_name(crate::types::type_name("fixture/Outer$Hidden$Context"))
            .classifier
            .is_none());
        // Every classifier API must report the enclosing source restriction. Returning the public
        // leaf visibility here would let the resolver's public fast path disagree with the type and
        // package-access queries above.
        assert_eq!(
            analysis
                .symbols
                .libraries
                .classifier_visibility(crate::types::type_name("fixture/Outer$Hidden$Context")),
            Some(Visibility::Internal)
        );
        assert_eq!(
            analysis
                .symbols
                .libraries
                .classifier_access(crate::types::type_name("fixture/Outer$Hidden$Context")),
            Some(crate::symbol_source::ClassifierAccess::Internal)
        );
        assert!(!analysis
            .symbols
            .libraries
            .classifier_accessible_from_package(
                crate::types::type_name("fixture/Outer$Hidden$Context"),
                crate::types::type_name("consumer"),
            ));
    }

    #[test]
    fn declaration_only_internal_ancestor_shadows_absent_platform_descendant() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import fixture.Outer\n\
                 val hidden: Outer.Hidden.Context? = null",
            ),
            SourceInput::kotlin("package fixture\nclass Outer { internal class Hidden }"),
        ];
        let mut diagnostics = DiagSink::new();

        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(ExistingLibrary),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        let hidden = crate::types::type_name("fixture/Outer$Hidden$Context");

        assert!(diagnostics.diags.iter().any(|diagnostic| {
            diagnostic
                .msg
                .contains("unresolved reference 'Outer.Hidden.Context'")
        }));
        assert!(analysis
            .symbols
            .libraries
            .resolve_type_name(hidden)
            .is_none());
        assert!(analysis
            .symbols
            .libraries
            .resolve_symbols_name(hidden)
            .classifier
            .is_none());
        // Although the leaf exists only on the platform, its source-declared internal owner claims
        // the path. The visibility/access APIs must carry that owner restriction instead of falling
        // through to the platform leaf and describing the same rejected type as public.
        assert_eq!(
            analysis.symbols.libraries.classifier_visibility(hidden),
            Some(Visibility::Internal)
        );
        assert_eq!(
            analysis.symbols.libraries.classifier_access(hidden),
            Some(crate::symbol_source::ClassifierAccess::Internal)
        );
    }

    #[test]
    fn declaration_only_public_ancestors_allow_absent_platform_descendant() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import fixture.Outer\n\
                 val visible: Outer.Hidden.Context? = null",
            ),
            SourceInput::kotlin("package fixture\nclass Outer { class Hidden }"),
        ];
        let mut diagnostics = DiagSink::new();

        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(ExistingLibrary),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        let visible = crate::types::type_name("fixture/Outer$Hidden$Context");

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        assert!(analysis
            .symbols
            .libraries
            .resolve_type_name(visible)
            .is_some());
    }

    #[test]
    fn declaration_only_internal_class_shadows_platform_static_field() {
        let inputs = [
            SourceInput::kotlin("package consumer\nval checked = Unit"),
            SourceInput::kotlin("package fixture\ninternal class CollisionEnum"),
        ];
        let mut diagnostics = DiagSink::new();

        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(ExistingLibrary),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        let collision = crate::types::type_name("fixture/CollisionEnum");

        assert_eq!(
            analysis.symbols.libraries.classifier_visibility(collision),
            Some(Visibility::Internal)
        );
        assert!(analysis
            .symbols
            .libraries
            .static_field("fixture/CollisionEnum", "ANY")
            .is_none());
        assert!(analysis
            .symbols
            .libraries
            .static_field_name(collision, "ANY")
            .is_none());
    }

    #[test]
    fn inferred_friend_sources_expose_internal_classifiers() {
        let inputs = [
            SourceInput::kotlin(
                "package consumer\n\
                 import dependency.Hidden\n\
                 fun hidden(): Any = Hidden()",
            ),
            SourceInput::kotlin("package dependency\ninternal class Hidden"),
        ];
        let mut diagnostics = DiagSink::new();

        analyze_source_set_prefix_with_features(
            &inputs,
            1,
            2,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    }

    #[test]
    fn dependency_fallback_exposes_inherited_nested_classifier_to_subclass() {
        let features = LangFeatures::new();
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_prefix_with_features(
            &[
                SourceInput::kotlin(
                    "package consumer\n\
                     import support.Parent\n\
                     class Child(category: Category) : Parent()",
                ),
                SourceInput::kotlin(
                    "package support\n\
                     open class Parent { enum class Category { FIRST } }",
                ),
            ],
            1,
            1,
            Box::new(EmptySymbolSource),
            &features,
            &mut diagnostics,
        );

        assert!(
            analysis.types[0].is_some() && diagnostics.diags.is_empty(),
            "{:?}",
            diagnostics.diags
        );
        assert_eq!(
            analysis
                .symbols
                .classes
                .get(&crate::types::type_name("consumer/Child"))
                .expect("consumer class")
                .ctor_params,
            [Ty::obj("support/Parent$Category")]
        );
    }

    #[test]
    fn dependency_fallback_preserves_protected_classifier_for_subclass_only() {
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_prefix_with_features(
            &[
                SourceInput::kotlin(
                    "package consumer\n\
                     import support.Parent\n\
                     class Child : Parent() {\n\
                         fun String.read(): String =\n\
                             Category(\"O\").value() + Category(second = \"K\").value()\n\
                         fun value(): String = \"\".read()\n\
                     }",
                ),
                SourceInput::kotlin(
                    "package support\n\
                     open class Parent {\n\
                         protected class Category(\n\
                             private val first: String = \"O\",\n\
                             private val second: String = \"K\",\n\
                         ) { fun value(): String = first + second }\n\
                     }",
                ),
            ],
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(
            analysis.types[0].is_some() && diagnostics.diags.is_empty(),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn dependency_fallback_does_not_globally_expose_protected_classifier() {
        let mut diagnostics = DiagSink::new();
        analyze_source_set_prefix_with_features(
            &[
                SourceInput::kotlin(
                    "package consumer\n\
                     import support.Parent\n\
                     class Unrelated { fun make(): Any = Category() }",
                ),
                SourceInput::kotlin(
                    "package support\n\
                     open class Parent { protected class Category }",
                ),
            ],
            1,
            1,
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(
            diagnostics
                .diags
                .iter()
                .any(|diagnostic| diagnostic.msg.contains("Category")),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn dependency_fallback_does_not_expose_nested_classifier_outside_subclass() {
        let features = LangFeatures::new();
        let mut diagnostics = DiagSink::new();
        analyze_source_set_prefix_with_features(
            &[
                SourceInput::kotlin(
                    "package consumer\n\
                     import support.Parent\n\
                     class Unrelated(category: Category)",
                ),
                SourceInput::kotlin(
                    "package support\n\
                     open class Parent { enum class Category { FIRST } }",
                ),
            ],
            1,
            1,
            Box::new(EmptySymbolSource),
            &features,
            &mut diagnostics,
        );

        assert!(
            diagnostics
                .diags
                .iter()
                .any(|diagnostic| diagnostic.msg.contains("unresolved reference 'Category'")),
            "{:?}",
            diagnostics.diags
        );
    }

    #[test]
    fn dependency_fallback_keeps_a_source_property_missing_from_the_public_api() {
        let features = LangFeatures::new();
        let inputs = [
            SourceInput::kotlin(
                "package feature\n\
                 import fixture.Container\n\
                 fun use(): Int = Container.Labels.marker",
            ),
            SourceInput::kotlin(
                "package fixture\n\
                 class Container {\n\
                     object Labels { val marker: Int = 1 }\n\
                 }",
            ),
        ];
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            1,
            1,
            Box::new(ExistingLibrary),
            &features,
            &mut diagnostics,
        );
        assert!(
            analysis.types[0].is_some() && diagnostics.diags.is_empty(),
            "{:?}",
            diagnostics.diags
        );
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
            editor_span: None,
            severity: Severity::Warning,
            kind: crate::diag::DiagnosticKind::Compiler,
            msg: "existing warning".to_string(),
            identity: None,
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
