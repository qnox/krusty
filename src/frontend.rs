//! Frontend entry points.
//!
//! Source analysis: lexing, parsing, signature collection, and checking.

use crate::ast::File;
use crate::diag::{DiagSink, Severity, Span};
use crate::features::LangFeatures;
pub use crate::fir::DeclarationId as FrontendDeclarationId;
pub use crate::lexer::{NameToken as FrontendNameToken, NameTokenKind as FrontendNameTokenKind};
use crate::libraries::{EmptySymbolSource, SemanticPlatform};

mod header_validation;
mod inline_preparation;
mod retained_syntax;
pub(crate) use crate::resolve::class_internal_resolver;
pub use crate::resolve::ClassFlags as FrontendClassFlags;
pub(crate) use crate::resolve::ClassModel as FrontendClassModel;
pub(crate) use crate::resolve::ClassSig as FrontendClassSig;
pub(crate) use crate::resolve::DeclaredPropertySig as FrontendDeclaredPropertySig;
pub use crate::resolve::ExtPropSig as FrontendExtPropSig;
pub use crate::resolve::SymbolTable as FrontendSymbols;
pub use crate::resolve::TypeInfo as FrontendTypeInfo;
pub use crate::resolve::{
    check_file, check_file_at, check_file_in_source_set, collect_signatures,
    collect_signatures_with_cp, AnonymousObjectCapture, AnonymousObjectCaptureSource,
    CompoundAssignmentTarget, SourceConstructorMatcher,
};
pub(crate) use crate::resolve::{
    check_preinferred_file_in_source_set, AdaptedRefArgument, CallableReferenceBinding,
    CallableReferenceTarget, ConstructorReferenceOuter, CtorDefaultValue, DelegateGetValueTarget,
    DestructureComponentTarget, ExprLowering, ImplicitPropertyWriteTarget,
    ImplicitReceiverSelection, IncDecSite, InvokeKind, LambdaCapture, LambdaInfo,
    PlatformNarrowing, ReceiverFnValueOrigin, ResolvedCall, ResolvedConstructor,
    ResolvedContextArgument, ResolvedCtorDelegationTarget, ResolvedExtensionCall, ResolvedIncDec,
    ResolvedLocalFunctionCall, ResolvedMember, ResolvedPropertyAccess, ResolvedSuperCall,
    ResolvedTopLevelCall, ResolvedTopLevelFunctionRef, ReturnTarget, SigFlags, Signature,
    SingletonValue, StmtLowering,
};
pub(crate) use crate::resolve::{selected_context_values, SelectedContextSources};
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

/// Analysis result for a jointly compiled source set.
///
/// Inspection-oriented entry points retain `files`/`types`. Emission-oriented analysis deliberately
/// returns those vectors empty after inline FIR preparation; Pass 2 reparses through
/// `reparse_sources` and never observes the Pass-1 arenas.
pub struct SourceSetAnalysis {
    pub files: Vec<File>,
    pub symbols: FrontendSymbols,
    pub types: Vec<Option<FrontendTypeInfo>>,
    pub(crate) reparse_sources: Vec<ReparseSource>,
    pub(crate) streamed: Option<StreamedPassState>,
}

/// Production result after Pass 1 has been consumed.
///
/// Unlike [`SourceSetAnalysis`], this type has no slot for parsed files, AST-keyed type tables, or
/// the Pass-1 signature graph.  A successful value owns only the external semantic platform, the
/// finalized FIR module, and the source text that the driver will sequentially reparse in Pass 2.
pub struct StreamingSourceSetAnalysis {
    pub(crate) symbols: crate::resolve::PassTwoSymbols,
    pub(crate) reparse_sources: Vec<ReparseSource>,
    pub(crate) streamed: Option<StreamedPassState>,
}

impl From<SourceSetAnalysis> for StreamingSourceSetAnalysis {
    fn from(analysis: SourceSetAnalysis) -> Self {
        let SourceSetAnalysis {
            files,
            symbols,
            types,
            reparse_sources,
            streamed,
        } = analysis;
        drop(files);
        drop(types);
        let symbols = symbols.into_pass_two_symbols();
        Self {
            symbols,
            reparse_sources,
            streamed,
        }
    }
}

/// Owned source input used only to materialize one active Pass-2 file. It is compilation
/// orchestration state, not part of [`crate::fir::FrontendModule`], and cannot retain an AST.
pub(crate) struct ReparseSource {
    kind: SourceKind,
    is_common: bool,
    text: Box<str>,
    file_stem: Option<Box<str>>,
    features: LangFeatures,
    #[cfg(test)]
    parse_count: std::cell::Cell<usize>,
    #[cfg(test)]
    released_before_collection: bool,
}

impl ReparseSource {
    pub(crate) fn visit_declaration_units(
        &self,
        diags: &mut DiagSink,
        mut visit: impl FnMut(File, &mut DiagSink),
    ) {
        assert_eq!(
            self.kind,
            SourceKind::Kotlin,
            "production declaration-unit streaming accepts Kotlin files only"
        );
        #[cfg(test)]
        self.parse_count.set(self.parse_count.get() + 1);
        let tokens = crate::lexer::lex(&self.text, diags);
        let mut anonymous_counters = std::collections::HashMap::new();
        crate::parser::visit_declaration_units_with_features(
            &self.text,
            &tokens,
            diags,
            &self.features,
            |mut file, diags| {
                file.is_common = self.is_common;
                if let Some(stem) = self.file_stem.as_deref() {
                    name_anonymous_classes_with_counters(
                        &mut file,
                        &format!("{stem}Kt"),
                        &mut anonymous_counters,
                    );
                }
                visit(file, diags);
            },
        );
    }

    pub(crate) fn is_script(&self) -> bool {
        self.kind == SourceKind::KotlinScript
    }

    pub(crate) fn is_java(&self) -> bool {
        self.kind == SourceKind::Java
    }

    /// Run the ordinary checker during Pass 2 when Pass 1 published only a partial semantic index.
    /// Invalid modules never enter FIR/lowering, but Kotlin still requires diagnostics from every
    /// independently checkable body. This is still the second source parse and the transient file
    /// drops before the next source. Stable declaration inventory binds each fresh parser unit; no
    /// Pass-1 parser coordinate or signature graph survives. Java bodies remain owned by javac.
    pub(crate) fn visit_diagnostic_units(
        &self,
        diags: &mut DiagSink,
        mut visit: impl FnMut(File, &mut DiagSink),
    ) {
        match self.kind {
            // Invalid stable signatures cannot enter checked FIR, but ordinary Kotlin declarations
            // still recover diagnostics through the same bounded Pass-2 parser used by successful
            // modules. Keeping the whole reparsed file alive here would make error recovery a hidden
            // non-streaming architecture.
            SourceKind::Kotlin => self.visit_declaration_units(diags, visit),
            SourceKind::KotlinScript => {
                #[cfg(test)]
                self.parse_count.set(self.parse_count.get() + 1);
                let mut file = parse_source_kind(&self.text, self.kind, &self.features, diags);
                file.is_common = self.is_common;
                visit(file, diags);
            }
            SourceKind::Java => {}
        }
    }

    #[cfg(test)]
    fn parse_count(&self) -> usize {
        self.parse_count.get()
    }

    #[cfg(test)]
    fn released_before_collection(&self) -> bool {
        self.released_before_collection
    }
}

/// Connected stable-identity product of Pass 1. It is kept beside the legacy AST result only while
/// callers migrate; none of its fields can retain syntax or a temporary signature graph.
pub(crate) struct StreamedPassState {
    pub module: crate::fir::FrontendModule,
    pub diagnostic_recovery: bool,
    /// Declarations Pass 1 declined without any diagnostic of their own. Reported by
    /// [`report_declined_signatures`] once Pass 2 has had its say, so a real body diagnostic inside
    /// the declaration stays the only message.
    pub declined: Vec<DeclinedSignature>,
}

/// A declaration whose signature Pass 1 could not finalize and for which nothing was reported.
///
/// Left alone, such a declaration vanishes: Pass 2 has no signature to check its uses against, the
/// batch compiler exits 0 having emitted nothing for its file, and the editor shows only the
/// cascade at every use. The record keeps the declaration's own coordinates so the report can be
/// deferred until after Pass 2 and skipped when that pass explained the failure itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclinedSignature {
    pub file: u32,
    pub span: Span,
    pub message: String,
}

/// Report every declined signature in a file that no error explains.
///
/// The granularity is the FILE: a decline usually stems from a failure the solver cannot link
/// back (a receiver whose own initializer was rejected, a dependency declined earlier), and kotlinc
/// reports only the root there. A file with an error is already known to be broken and gets no
/// second message; a file with none would otherwise pass as clean while its declarations went
/// unchecked and unemitted.
pub fn report_declined_signatures(declined: &[DeclinedSignature], diags: &mut DiagSink) {
    for decline in declined {
        let explained = diags.diags.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error && diagnostic.file == decline.file
        });
        if explained {
            continue;
        }
        diags.set_file(decline.file);
        diags.error(decline.span, decline.message.clone());
    }
}

fn diagnostic_streamed_state(
    mut index: crate::fir::ResolvedModuleIndex,
    sources: crate::fir::SourceMap,
    declined: Vec<DeclinedSignature>,
) -> StreamedPassState {
    index.release_source_coordinates();
    assert!(
        !index.retains_source_coordinates(),
        "diagnostic Pass 2 must not retain Pass-1 source coordinates"
    );
    StreamedPassState {
        module: crate::fir::FrontendModule::new(
            index,
            crate::fir::InlineBodyStore::default(),
            crate::fir::DefaultArgumentStore::default(),
            sources,
        ),
        diagnostic_recovery: true,
        declined,
    }
}

#[cfg(test)]
impl StreamedPassState {
    /// Same-parse adapter for focused FIR unit tests. Production discovers bodies only inside the
    /// bounded Pass-2 callback; tests that intentionally retain a whole AST can enumerate that live
    /// syntax without recreating the removed cross-pass body queue.
    pub(crate) fn ordinary_body_work(
        &self,
        file: &File,
        source: crate::fir::SourceFileId,
    ) -> Vec<crate::fir::BodyWorkItem> {
        let mut cursor = crate::fir::ActiveSourceCursor::new(source, self.module.index());
        let active = cursor
            .bind_next(file, source, self.module.index())
            .expect("test AST must bind to the stable declaration stream");
        assert!(cursor.is_finished(), "test AST must consume every header");
        active
            .ordinary_body_work(file, source, self.module.index())
            .expect("test AST bodies must bind to stable declarations")
    }
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

fn expect_key(file: &File, id: crate::ast::DeclId) -> ExpectKey {
    let pkg = file.package.clone().unwrap_or_default();
    let (kind, name, recv, arity) = match file.decl(id) {
        crate::ast::Decl::Fun(function) => (
            0,
            function.name.clone(),
            function
                .receiver
                .as_ref()
                .map(|receiver| receiver.name.clone())
                .unwrap_or_default(),
            function.params.len(),
        ),
        crate::ast::Decl::Class(class) => (1, class.name.clone(), String::new(), 0),
        crate::ast::Decl::Property(property) => (
            2,
            property.name.clone(),
            property
                .receiver
                .as_ref()
                .map(|receiver| receiver.name.clone())
                .unwrap_or_default(),
            0,
        ),
    };
    (pkg, kind, name, recv, arity)
}

pub fn strip_matched_expects(files: &mut [File]) {
    // The match key is PACKAGE-qualified (expect/actual couple by FqName) but deliberately omits
    // the RETURN/property type and the receiver's TYPE ARGUMENTS (`List<String>.foo` keys as
    // `List`) — an `actual` routinely INFERS it (`actual fun greet() = "O"`), so a
    // type component would wrongly leave such pairs unmatched. kotlinc validates actual/expect
    // compatibility upstream; krusty trusts that and lets an incompatible pair fail checking on
    // its own terms downstream.
    // Actualization stage A: every NON-expect top-level declaration's key across the whole set. An
    // `actual typealias S = String` also actualizes an `expect class S` — typealiases live in
    // `File.type_aliases`, so add each alias NAME as a class-kind actual.
    let mut actuals: std::collections::HashSet<ExpectKey> = std::collections::HashSet::new();
    for file in files.iter() {
        for &d in &file.decls {
            if !file.expect_decls.contains(&d) {
                actuals.insert(expect_key(file, d));
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
    let matched = files
        .iter()
        .enumerate()
        .flat_map(|(file_index, file)| {
            file.expect_decls.iter().copied().filter_map({
                let actuals = &actuals;
                move |declaration| {
                    actuals
                        .contains(&expect_key(file, declaration))
                        .then_some((file_index as u32, declaration))
                }
            })
        })
        .collect::<std::collections::HashSet<_>>();
    strip_selected_expects(files, &matched);
}

fn strip_selected_expects(
    files: &mut [File],
    matched: &std::collections::HashSet<(u32, crate::ast::DeclId)>,
) {
    // Actualization removes only declarations selected by compact stable-header matching. Defaults
    // are checked from the still-live expect syntax and stored as target-owned FIR in Pass 1.
    for (file_index, file) in files.iter_mut().enumerate() {
        let expects = std::mem::take(&mut file.expect_decls);
        let drop: Vec<crate::ast::DeclId> = expects
            .iter()
            .filter(|&&declaration| matched.contains(&(file_index as u32, declaration)))
            .copied()
            .collect();
        file.decls.retain(|d| !drop.contains(d));
        file.expect_decls = expects.into_iter().filter(|d| !drop.contains(d)).collect();
    }
}

/// Publish expect-owned default presence on surviving actual headers and return the stable
/// provider→target work. Matched expect syntax remains in the active Pass-1 parser stream only
/// until those defaults become checked FIR; compact-header exclusion keeps it out of signatures.
fn actualize_headers_and_collect_inherited_defaults(
    headers: &mut crate::fir::StreamedHeaderModule,
) -> (
    std::collections::HashSet<crate::fir::DeclarationId>,
    Vec<crate::fir::DefaultArgumentProvider>,
) {
    let inherited_defaults = crate::fir::actualized_declaration_pairs(headers)
        .into_iter()
        .filter_map(|pair| {
            let source_parameters = match headers.syntax.declaration(pair.expect)?.kind {
                crate::fir::HeaderDeclarationKind::Callable { parameters, .. }
                | crate::fir::HeaderDeclarationKind::Constructor { parameters, .. } => parameters,
                _ => return None,
            };
            let target_parameters = match headers.syntax.declaration(pair.actual)?.kind {
                crate::fir::HeaderDeclarationKind::Callable { parameters, .. }
                | crate::fir::HeaderDeclarationKind::Constructor { parameters, .. } => parameters,
                _ => return None,
            };
            let defaults = headers
                .syntax
                .parameters(source_parameters)
                .iter()
                .map(|parameter| parameter.flags.has_default())
                .collect::<Vec<_>>();
            (headers.syntax.parameters(target_parameters).len() == defaults.len()
                && defaults.iter().any(|default| *default))
            .then_some((pair, target_parameters, defaults))
        })
        .collect::<Vec<_>>();
    let work = inherited_defaults
        .iter()
        .map(|(pair, _, _)| crate::fir::DefaultArgumentProvider {
            target: pair.actual,
            provider: pair.expect,
            relation: crate::fir::DefaultArgumentRelation::ActualizedDeclaration,
        })
        .collect::<Vec<_>>();
    for (_, parameters, defaults) in inherited_defaults {
        headers.syntax.set_parameter_defaults(parameters, &defaults);
    }
    // Keep matched expect syntax in the active Pass-1 parser stream. Compact-header exclusion is
    // already authoritative for signature collection, while inherited defaults still need their
    // provider declaration long enough to become checked target-owned FIR. Removing the parser
    // declaration here forced later code to recover it by `(file, TextRange)`.
    (crate::fir::matched_expect_declarations(headers), work)
}

/// Reject every top-level expect subtree for which compact actualization found no platform root.
/// This is a source-set semantic check, not a consequence of whether a particular expect spelling
/// happens to have an executable body. Reporting it before exclusion also prevents body checking
/// or a backend from accidentally treating a body-less expect function as an abstract declaration.
fn report_unmatched_expect_roots(
    headers: &crate::fir::StreamedHeaderModule,
    matched: &std::collections::HashSet<crate::fir::DeclarationId>,
    symbols: &FrontendSymbols,
    rejected_sources: &mut [bool],
    diags: &mut DiagSink,
) {
    for stub in headers.stubs.iter().filter(|stub| {
        stub.flags.has(crate::fir::DeclarationFlags::EXPECT)
            && headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner.is_none())
            && !matched.contains(&stub.id)
            && !symbols.is_source_optional_expectation(stub.id)
    }) {
        let source = stub.source.raw() as usize;
        if let Some(rejected) = rejected_sources.get_mut(source) {
            *rejected = true;
        }
        diags.set_file(stub.source.raw());
        let name = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
            .unwrap_or("<anonymous>");
        diags.error(
            stub.range,
            format!("expected declaration '{name}' has no actual declaration in this module"),
        );
    }
}

fn signature_default_work(
    headers: &crate::fir::StreamedHeaderModule,
    inherited: &[crate::fir::DefaultArgumentProvider],
) -> Vec<crate::fir::DefaultArgumentProvider> {
    let inherited_declarations = inherited
        .iter()
        .flat_map(|work| [work.provider, work.target])
        .collect::<std::collections::HashSet<_>>();
    let mut work = headers
        .stubs
        .iter()
        .filter(|stub| !inherited_declarations.contains(&stub.id))
        .filter_map(|stub| {
            let parameters = match headers.syntax.declaration(stub.id)?.kind {
                crate::fir::HeaderDeclarationKind::Callable { parameters, .. }
                | crate::fir::HeaderDeclarationKind::Constructor { parameters, .. } => parameters,
                _ => return None,
            };
            headers
                .syntax
                .parameters(parameters)
                .iter()
                .any(|parameter| parameter.flags.has_default())
                .then_some(crate::fir::DefaultArgumentProvider {
                    target: stub.id,
                    provider: stub.id,
                    relation: crate::fir::DefaultArgumentRelation::SameDeclaration,
                })
        })
        .collect::<Vec<_>>();
    work.extend_from_slice(inherited);
    work.sort_by_key(|work| (work.provider, work.target, work.relation));
    work.dedup();
    work
}

/// Extend the Pass-1 default store across exact module override edges. The overriding callable is
/// the semantic call target, while the nearest overridden declaration remains the expression
/// provider. Both are stable declaration identities; no source coordinate or reparsed body crosses
/// the pass boundary.
fn inherit_override_default_work(
    headers: &mut crate::fir::StreamedHeaderModule,
    index: &crate::fir::ResolvedModuleIndex,
    work: &mut Vec<crate::fir::DefaultArgumentProvider>,
) {
    let classifiers = headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == crate::fir::DeclarationKind::Classifier)
        .map(|stub| stub.id)
        .collect::<Vec<_>>();
    loop {
        let providers = work
            .iter()
            .map(|item| (item.target, item.provider))
            .collect::<std::collections::HashMap<_, _>>();
        let mut additions = Vec::new();
        for classifier in &classifiers {
            for edge in index.function_overrides(*classifier) {
                let (
                    crate::fir::ResolvedFunctionOverrideTarget::Module(implementation),
                    crate::fir::ResolvedFunctionOverrideTarget::Module(overridden),
                ) = (edge.implementation, edge.overridden)
                else {
                    continue;
                };
                let Some(target) = index
                    .callable(implementation)
                    .map(|callable| callable.declaration)
                else {
                    continue;
                };
                if providers.contains_key(&target)
                    || additions
                        .iter()
                        .any(|item: &crate::fir::DefaultArgumentProvider| item.target == target)
                {
                    continue;
                }
                let Some(overridden) = index
                    .callable(overridden)
                    .map(|callable| callable.declaration)
                else {
                    continue;
                };
                let Some(provider) = providers.get(&overridden).copied() else {
                    continue;
                };
                let defaults = match headers
                    .syntax
                    .declaration(provider)
                    .map(|declaration| declaration.kind)
                {
                    Some(crate::fir::HeaderDeclarationKind::Callable { parameters, .. })
                    | Some(crate::fir::HeaderDeclarationKind::Constructor { parameters, .. }) => {
                        headers
                            .syntax
                            .parameters(parameters)
                            .iter()
                            .map(|parameter| parameter.flags.has_default())
                            .collect::<Vec<_>>()
                    }
                    _ => continue,
                };
                let target_parameters = match headers
                    .syntax
                    .declaration(target)
                    .map(|declaration| declaration.kind)
                {
                    Some(crate::fir::HeaderDeclarationKind::Callable { parameters, .. })
                    | Some(crate::fir::HeaderDeclarationKind::Constructor { parameters, .. }) => {
                        parameters
                    }
                    _ => continue,
                };
                if defaults.len() != headers.syntax.parameters(target_parameters).len()
                    || !defaults.iter().any(|default| *default)
                {
                    continue;
                }
                headers
                    .syntax
                    .set_parameter_defaults(target_parameters, &defaults);
                additions.push(crate::fir::DefaultArgumentProvider {
                    target,
                    provider,
                    relation: crate::fir::DefaultArgumentRelation::InheritedOverride,
                });
            }
        }
        if additions.is_empty() {
            break;
        }
        work.extend(additions);
    }
    work.sort_by_key(|item| (item.provider, item.target, item.relation));
    work.dedup();
}

fn has_signature_defaults(file: &File) -> bool {
    file.decl_arena.iter().any(|declaration| match declaration {
        crate::ast::Decl::Fun(function) => function
            .params
            .iter()
            .any(|parameter| parameter.default.is_some()),
        crate::ast::Decl::Property(_) => false,
        crate::ast::Decl::Class(class) => {
            class
                .props
                .iter()
                .any(|parameter| parameter.default.is_some())
                || class.methods.iter().any(|method| {
                    method
                        .params
                        .iter()
                        .any(|parameter| parameter.default.is_some())
                })
                || class.secondary_ctors.iter().any(|constructor| {
                    constructor
                        .params
                        .iter()
                        .any(|parameter| parameter.default.is_some())
                })
                || class.enum_entries.iter().any(|entry| {
                    entry.methods.iter().any(|method| {
                        method
                            .params
                            .iter()
                            .any(|parameter| parameter.default.is_some())
                    })
                })
        }
    })
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
    analyze_source_set_impl(
        sources,
        sources.len(),
        sources.len(),
        platform,
        project_features,
        |_, _| {},
        diags,
        false,
        true,
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
    analyze_source_set_impl(
        sources,
        checked_count,
        inferred_count,
        platform,
        project_features,
        |_, _| {},
        diags,
        false,
        true,
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

/// Analyze a source set for the production two-pass pipeline without allowing a target to mutate
/// frontend semantic state. Target layout is realized only after checked common IR exists; file
/// containers, physical names, and descriptors therefore cannot influence Pass-1 signature
/// solving or Pass-2 body checking.
pub fn analyze_source_set_streaming_with_features(
    sources: &[SourceInput<'_>],
    platform: Box<dyn SemanticPlatform>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> StreamingSourceSetAnalysis {
    analyze_source_set_impl(
        sources,
        sources.len(),
        sources.len(),
        platform,
        project_features,
        |_, _| {},
        diags,
        false,
        false,
    )
    .into()
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
    retain_legacy_analysis: bool,
) -> SourceSetAnalysis
where
    F: FnOnce(&[File], &mut FrontendSymbols),
{
    let diagnostics_start = diags.diags.len();
    let mut files = Vec::with_capacity(sources.len());
    let mut parse_errors = Vec::with_capacity(sources.len());
    let mut reparse_sources = Vec::with_capacity(sources.len());
    let mut pass1_builder = crate::fir::HeaderInventoryBuilder::default();
    let mut signature_constraints = crate::fir::SignatureConstraintExtractor::default();
    let mut source_contracts = Vec::new();
    let mut local_class_contexts = Vec::with_capacity(sources.len());
    // Decide this before parsing the first file. A per-source language directive in a later file
    // applies to the jointly compiled source set; discovering it incrementally would let an earlier
    // expect/default body be released before actualization has harvested it.
    let multiplatform = project_features.has("MultiPlatformProjects")
        || sources.iter().any(|source| {
            let mut features = project_features.clone();
            features.apply_source_directives(source.text);
            features.has("MultiPlatformProjects")
        });
    for (index, source) in sources.iter().enumerate() {
        diags.set_file(index as u32);
        let mut features = project_features.clone();
        features.apply_source_directives(source.text);
        reparse_sources.push(ReparseSource {
            kind: source.kind,
            is_common: source.is_common,
            text: source.text.into(),
            file_stem: source.file_stem.map(Into::into),
            features: features.clone(),
            #[cfg(test)]
            parse_count: std::cell::Cell::new(0),
            #[cfg(test)]
            released_before_collection: false,
        });
        let diagnostics_before = diags.diags.len();
        let mut file = parse_source_kind(source.text, source.kind, &features, diags);
        file.is_common = source.is_common;
        if source.kind == SourceKind::Kotlin {
            if let Some(stem) = source.file_stem {
                name_anonymous_classes(&mut file, &format!("{stem}Kt"));
            }
            header_validation::validate(&file, diags);
        }
        let parse_error = source.kind != SourceKind::Java
            && diags.diags[diagnostics_before..]
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error);
        parse_errors.push(parse_error);
        let extracted = pass1_builder.add_source(
            index,
            source,
            (index < inferred_count && !parse_error && source.kind != SourceKind::Java)
                .then_some(&file),
        );
        if let Some((source, stubs)) = extracted {
            source_contracts.extend(crate::resolve::extract_source_contract_candidates(
                &file, source, &stubs,
            ));
            signature_constraints.extract_file(&file, source, &stubs, |span| {
                pass1_builder.source_origin(source, span)
            });
            // Compact signature extraction has consumed every ordinary expression dependency for
            // this source. Production keeps the parser body arenas only for bounded Pass-1 work
            // that has not moved to its own store yet: inline checking, const evaluation, and MPP
            // actualization matching. Dependency-prefix and inspection entry points retain their
            // legacy view until their separate migration boundary.
            let needs_bounded_pass_one_syntax = multiplatform
                || has_signature_defaults(&file)
                || stubs.iter().any(|stub| {
                    stub.flags.has(crate::fir::DeclarationFlags::INLINE)
                        || stub.flags.has(crate::fir::DeclarationFlags::CONST)
                });
            local_class_contexts.push(crate::resolve::pass_one_local_class_context(&file, &stubs));
            if !retain_legacy_analysis && index < inferred_count && !multiplatform {
                if needs_bounded_pass_one_syntax {
                    retained_syntax::compact(&mut file);
                } else {
                    file.release_body_arenas();
                }
                #[cfg(test)]
                {
                    reparse_sources
                        .last_mut()
                        .expect("the active source owns reparse state")
                        .released_before_collection = true;
                }
            }
        } else {
            local_class_contexts.push(crate::resolve::pass_one_local_class_context(&file, &[]));
        }
        files.push(file);
    }

    assert!(checked_count <= inferred_count && inferred_count <= files.len());
    let mut pass1_headers = pass1_builder.finish();
    let source_classifiers = pass1_headers.source_classifier_names();
    let platform_sources = sources
        .iter()
        .enumerate()
        .filter(|(_, source)| source.kind == SourceKind::Java)
        .map(
            |(source, input)| crate::libraries::PlatformSourceHeaderInput {
                source,
                file_stem: input.file_stem,
                text: input.text,
            },
        )
        .collect::<Vec<_>>();
    if let Err(error) =
        platform.install_source_module_headers(&platform_sources, &source_classifiers)
    {
        diags.set_file(error.source as u32);
        diags.error(Span::new(0, 0), error.message);
    }
    let (mut signature_default_work_items, matched_expect_declarations) = if multiplatform {
        let (matched, defaults) =
            actualize_headers_and_collect_inherited_defaults(&mut pass1_headers);
        pass1_headers.exclude_declaration_subtrees(&matched);
        // Explicit expect→actual default mappings remain valid after exclusion because their
        // provider anchors and bounded syntax live through the rest of Pass 1. Enumerate ordinary
        // self-owned defaults only after exclusion so a removed expect constructor cannot schedule
        // an orphan target with no surviving signature or callable.
        let signature_default_work_items = signature_default_work(&pass1_headers, &defaults);
        // Actualization publishes stable expect-default providers before syntax is compacted. Once
        // that source-set operation is complete, retain only Pass-1 signature/inline fragments.
        if !retain_legacy_analysis {
            for (file, _source) in files
                .iter_mut()
                .zip(&mut reparse_sources)
                .take(inferred_count)
            {
                retained_syntax::compact(file);
                #[cfg(test)]
                {
                    _source.released_before_collection = true;
                }
            }
        }
        (signature_default_work_items, matched)
    } else {
        (
            signature_default_work(&pass1_headers, &[]),
            std::collections::HashSet::new(),
        )
    };
    let platform = if inferred_count < files.len() {
        let mut dependency_diags = DiagSink::new();
        let mut dependency_symbols =
            collect_signatures_with_cp(&files[inferred_count..], platform, &mut dependency_diags);
        dependency_symbols.offset_source_files(inferred_count as u32);
        let platform = std::mem::replace(
            &mut dependency_symbols.libraries,
            Box::new(EmptySymbolSource),
        );
        Box::new(crate::resolve::DependencyPlatform::new(
            platform,
            dependency_symbols,
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
    let mut symbols = crate::resolve::collect_signatures_with_cp_headers_and_local_contexts(
        &files[..inferred_end],
        &pass1_headers,
        &local_class_contexts[..inferred_end],
        platform,
        diags,
    );
    if multiplatform {
        report_unmatched_expect_roots(
            &pass1_headers,
            &matched_expect_declarations,
            &symbols,
            &mut parse_errors,
            diags,
        );
    }
    prepare_symbols(&files, &mut symbols);
    crate::resolve::install_streamed_plugin_declarations(&mut pass1_headers, &mut symbols);
    let inline_capture_selection = pass1_headers.inline_body_ranges(files.len());
    let has_inline_capture_roots = inline_capture_selection
        .roots
        .iter()
        .take(inferred_end)
        .any(|roots| !roots.is_empty());
    if retain_legacy_analysis {
        crate::resolve::discover_anonymous_object_captures(&files[..inferred_end], &mut symbols);
    } else if has_inline_capture_roots {
        crate::resolve::discover_inline_anonymous_object_captures(
            &files[..inferred_end],
            &inline_capture_selection.roots[..inferred_end],
            &inline_capture_selection.bodies[..inferred_end],
            &mut symbols,
        );
    }
    if retain_legacy_analysis || has_inline_capture_roots {
        crate::resolve::install_streamed_anonymous_capture_declarations(
            &files[..inferred_end],
            &mut pass1_headers,
            &mut symbols,
        );
    }
    if !retain_legacy_analysis {
        // Signature collection, target preparation, and inline-capture projection are the last
        // consumers of declaration-only legacy `File` views. From here on, retain a parser fragment
        // only when it still owns executable syntax that Pass 1 must turn into checked FIR
        // (inline/default/const work). The compact headers and signature graph are authoritative for
        // every declaration fact used by finalization, including enum-entry member signatures.
        for file in files.iter_mut().take(inferred_end) {
            if file.expr_arena.is_empty() && file.stmt_arena.is_empty() {
                *file = File::default();
            }
        }
    }
    let streamed_index = crate::resolve::finalized_streamed_signature_index(
        &pass1_headers,
        &mut symbols,
        signature_constraints,
        source_contracts,
        diags,
    );
    let mut recovery_streamed = None;
    let pending_streamed = if streamed_index.failures.is_empty() {
        let mut index = streamed_index.index;
        // Finalized signatures and their stable declaration ancestry form one Pass-1 product.
        // Publish the inventory before deriving declaration-owned metadata such as enum-entry
        // override edges; those entries deliberately have no ordinary classifier header.
        pass1_headers.publish_declaration_inventory(&mut index);
        crate::resolve::project_finalized_signatures(&index, &mut symbols);
        crate::resolve::finalize_streamed_top_level_conflicts(&pass1_headers, &mut symbols, diags);
        // A `const val` initializer is a stable declaration dependency. Check each such bounded
        // fragment now, while Pass 1 still owns its AST and exact operator selections can be
        // consumed; retain only the folded payload before the signature graph and arenas die.
        crate::resolve::publish_checked_compile_time_constants(
            &files[..inferred_end],
            &mut symbols,
        );
        crate::resolve::publish_stable_declaration_metadata(&mut index, &symbols);
        crate::resolve::publish_override_plans(&mut index, &symbols);
        inherit_override_default_work(
            &mut pass1_headers,
            &index,
            &mut signature_default_work_items,
        );
        // Defaults are signature-owned executable fragments. Check and detach them before the
        // compact header environment is consumed; no provider root or source locator crosses
        // this boundary.
        let default_arguments = inline_preparation::defaults(
            &mut pass1_headers,
            &mut index,
            std::mem::take(&mut signature_default_work_items),
            &files[..inferred_end],
            &parse_errors,
            checked_count,
            &mut symbols,
            diags,
        );
        match default_arguments {
            Some(default_arguments) => {
                let (index, sources, body_work) = pass1_headers.finish(index);
                let bodies = body_work.partition_by_inline(&index);
                let module = crate::fir::FrontendModule::new(
                    index,
                    crate::fir::InlineBodyStore::default(),
                    crate::fir::DefaultArgumentStore::default(),
                    sources,
                );
                Some((module, bodies, default_arguments))
            }
            None => {
                let (index, sources, _) = pass1_headers.finish(index);
                recovery_streamed = Some(diagnostic_streamed_state(index, sources, Vec::new()));
                None
            }
        }
    } else {
        crate::trace_compiler!(
            "fir",
            "Pass 1 signature finalization failed for declarations {:?}",
            streamed_index.failures,
        );
        // Keep only successfully finalized declarations for diagnostic recovery. Failed
        // signatures are absent—not represented by `Pending` or `Error`—and the complete lazy
        // graph/header syntax is consumed here before the second source pass begins.
        let mut index = streamed_index.index;
        pass1_headers.publish_declaration_inventory(&mut index);
        // The declarations that DID finalize are still the module's facts. The legacy Pass-2
        // checker reads them only through this projection, so skipping it here left every
        // inferred property and return type unresolved beside one genuine signature error — an
        // editor's steady state — and reported each use of them as a further unresolved reference.
        // Projection skips declarations without a finalized signature, so nothing failed leaks in.
        crate::resolve::project_finalized_signatures(&index, &mut symbols);
        let (index, sources, _) = pass1_headers.finish(index);
        recovery_streamed = Some(diagnostic_streamed_state(
            index,
            sources,
            streamed_index.declined,
        ));
        None
    };
    if trim_support_bodies {
        for file in &mut files[checked_count.min(inferred_end)..inferred_end] {
            file.release_body_arenas();
        }
    }
    let (types, streamed) = if retain_legacy_analysis {
        let types =
            check_source_set_skipping(&files, &mut symbols, &parse_errors, checked_count, diags);
        // The legacy checker was this path's Pass 2; a decline it could not explain is reported
        // now, at the declaration, rather than left to surface as a cascade at every use.
        if let Some(recovery) = &recovery_streamed {
            report_declined_signatures(&recovery.declined, diags);
        }
        let streamed = pending_streamed.and_then(|(module, bodies, default_arguments)| {
            inline_preparation::from_checked_analysis(
                module,
                bodies,
                default_arguments,
                &files,
                &types,
                &mut symbols,
            )
        });
        (types, streamed)
    } else {
        // Inline preparation consumes the bounded syntax retained from the initial parse. It moves
        // checked inline FIR into `InlineBodyStore` and releases every remaining parser body arena;
        // there is no separate inline-source parse between the two source passes.
        let streamed = pending_streamed.and_then(|(module, bodies, default_arguments)| {
            inline_preparation::streaming(
                module,
                bodies,
                default_arguments,
                &mut files,
                &parse_errors,
                checked_count,
                &mut symbols,
                diags,
            )
        });
        (Vec::new(), streamed)
    };
    let streamed = streamed.or(recovery_streamed);
    diags.collapse_duplicates_from(diagnostics_start);
    let analysis = SourceSetAnalysis {
        files: if retain_legacy_analysis {
            files
        } else {
            Vec::new()
        },
        symbols,
        types,
        reparse_sources,
        streamed,
    };
    if let Some(streamed) = &analysis.streamed {
        assert!(
            streamed.module.index().declaration_count() >= streamed.module.index().len(),
            "resolved signatures must be owned by the stable declaration index",
        );
        assert!(
            streamed.module.sources().len() <= sources.len(),
            "the stable source map cannot grow beyond the input source set",
        );
    }
    analysis
}

fn check_source_set_skipping(
    files: &[File],
    symbols: &mut FrontendSymbols,
    skip: &[bool],
    checked_count: usize,
    diags: &mut DiagSink,
) -> Vec<Option<FrontendTypeInfo>> {
    let types = files
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index >= checked_count || skip.get(index).copied().unwrap_or(false) {
                None
            } else {
                diags.set_file(index as u32);
                Some(check_preinferred_file_in_source_set(
                    files,
                    index as u32,
                    symbols,
                    diags,
                ))
            }
        })
        .collect();
    types
}

/// Check a parsed source set whose signatures have already been collected.
pub fn check_source_set(
    files: &[File],
    symbols: &mut FrontendSymbols,
    diags: &mut DiagSink,
) -> Vec<Option<FrontendTypeInfo>> {
    let diagnostics_start = diags.diags.len();
    // Capture discovery, for the same reason as the other entry point: an anonymous object's capture
    // fields and constructor parameters are facts the backend needs before it can lower one at all.
    crate::resolve::discover_anonymous_object_captures(files, symbols);
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

/// Rename anonymous-object classes from the parse-time placeholder (`Anon$anon$<offset>`) to
/// kotlinc's enclosing-scoped spelling (`P2$Companion$build$1`): the innermost enclosing FUNCTION
/// body (a member's or a top-level one) names the scope, with a per-scope 1-based ordinal in
/// source order. Must run BEFORE checking — the checker records these internals in every type it
/// hands the backend. A construction outside a function uses its innermost enclosing classifier, or
/// the file facade at top level. No parse-time placeholder may survive as semantic identity: offsets
/// repeat across files and would make unrelated anonymous classifiers overwrite one another.
pub fn name_anonymous_classes(file: &mut crate::ast::File, facade_simple: &str) {
    let mut counters = std::collections::HashMap::new();
    name_anonymous_classes_with_counters(file, facade_simple, &mut counters);
}

fn name_anonymous_classes_with_counters(
    file: &mut crate::ast::File,
    facade_simple: &str,
    counters: &mut std::collections::HashMap<String, u32>,
) {
    use crate::ast::{Decl, Expr};
    let mut anons: Vec<(crate::ast::ExprId, crate::ast::DeclId)> = file
        .anonymous_object_classes
        .iter()
        .map(|(&construction, &decl)| (construction, decl))
        .collect();
    anons.sort_by_key(|(construction, _)| file.expr_spans[construction.0 as usize].lo);
    for (construction, decl) in anons {
        let span = file.expr_spans[construction.0 as usize];
        let mut best: Option<(u32, String, crate::ast::AnonymousEnclosingFunction)> = None;
        for &candidate in &file.decls {
            match file.decl(candidate) {
                Decl::Fun(function) => {
                    if function.span.lo <= span.lo && span.hi <= function.span.hi {
                        let size = function.span.hi - function.span.lo;
                        if best
                            .as_ref()
                            .is_none_or(|(smallest, _, _)| size < *smallest)
                        {
                            best = Some((
                                size,
                                format!("{facade_simple}${}", function.name),
                                crate::ast::AnonymousEnclosingFunction::TopLevel(candidate),
                            ));
                        }
                    }
                }
                Decl::Class(class) => {
                    if candidate == decl {
                        continue;
                    }
                    let chain = class.name.replace('.', "$");
                    for (method_index, method) in class.methods.iter().enumerate() {
                        if method.span.lo <= span.lo && span.hi <= method.span.hi {
                            let size = method.span.hi - method.span.lo;
                            if best
                                .as_ref()
                                .is_none_or(|(smallest, _, _)| size < *smallest)
                            {
                                best = Some((
                                    size,
                                    format!("{chain}${}", method.name),
                                    crate::ast::AnonymousEnclosingFunction::Member {
                                        class: candidate,
                                        method: method_index as u32,
                                    },
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let (scope, enclosing) = match best {
            Some((_, scope, enclosing)) => (scope, Some(enclosing)),
            None => {
                let classifier_scope = file
                    .decls
                    .iter()
                    .filter_map(|&candidate| match file.decl(candidate) {
                        Decl::Class(class)
                            if candidate != decl
                                && class.span.lo <= span.lo
                                && span.hi <= class.span.hi =>
                        {
                            Some((class.span.hi - class.span.lo, class.name.replace('.', "$")))
                        }
                        Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
                    })
                    .min_by_key(|(size, _)| *size)
                    .map(|(_, scope)| scope)
                    .unwrap_or_else(|| facade_simple.to_string());
                (classifier_scope, None)
            }
        };
        if let Some(enclosing) = enclosing {
            file.anonymous_object_enclosing_functions
                .insert(decl, enclosing);
        }
        let ordinal = counters.entry(scope.clone()).or_insert(0);
        *ordinal += 1;
        let fresh = format!("{scope}${ordinal}");
        let Expr::Call { callee, .. } = file.expr(construction) else {
            continue;
        };
        let callee = *callee;
        let old = match file.decl(decl) {
            Decl::Class(class) => class.name.clone(),
            Decl::Fun(_) | Decl::Property(_) => continue,
        };
        let declarations = file.decls.clone();
        let class_ownership = declarations
            .iter()
            .filter_map(|declaration| match file.decl(*declaration) {
                Decl::Class(class) => Some((class.name.clone(), class.inner_of.clone())),
                Decl::Fun(_) | Decl::Property(_) => None,
            })
            .collect::<Vec<_>>();
        let mut renamed = std::collections::HashMap::from([(old.clone(), fresh.clone())]);
        for _ in 0..class_ownership.len() {
            let mut changed = false;
            for (name, owner) in &class_ownership {
                if renamed.contains_key(name) {
                    continue;
                }
                let Some(owner) = owner else { continue };
                let Some(new_owner) = renamed.get(owner) else {
                    continue;
                };
                let simple = name.rsplit('.').next().unwrap_or(name);
                renamed.insert(name.clone(), format!("{new_owner}.{simple}"));
                changed = true;
            }
            if !changed {
                break;
            }
        }
        for declaration in declarations {
            let Decl::Class(class) = file.decl_mut(declaration) else {
                continue;
            };
            if let Some(name) = renamed.get(&class.name) {
                class.name = name.clone();
            }
            if let Some(owner) = class.inner_of.as_mut() {
                if let Some(name) = renamed.get(owner) {
                    *owner = name.clone();
                }
            }
        }
        if let Expr::Name(name) = &mut file.expr_arena[callee.0 as usize] {
            *name = fresh;
        }
    }
}

#[cfg(test)]
mod tests;
