//! Frontend entry points.
//!
//! Source analysis: lexing, parsing, signature collection, and checking.

use crate::ast::File;
use crate::diag::{DiagSink, Severity, Span};
use crate::features::LangFeatures;
pub use crate::fir::DeclarationId as FrontendDeclarationId;
pub use crate::lexer::{NameToken as FrontendNameToken, NameTokenKind as FrontendNameTokenKind};
use crate::libraries::{EmptySymbolSource, SemanticPlatform};
use crate::plugins::registry::NativePlugins;

mod header_validation;
mod inline_preparation;
mod local_class_names;
mod local_function_names;
mod no_expect_for_actual;
mod retained_syntax;
pub use crate::resolve::ClassFlags as FrontendClassFlags;
pub(crate) use crate::resolve::ClassSig as FrontendClassSig;
pub(crate) use crate::resolve::DeclaredPropertySig as FrontendDeclaredPropertySig;
pub use crate::resolve::ExtPropSig as FrontendExtPropSig;
pub(crate) use crate::resolve::Signature;
pub use crate::resolve::SymbolTable as FrontendSymbols;
pub use crate::resolve::TypeInfo as FrontendTypeInfo;
pub use crate::resolve::{
    AnonymousObjectCapture, AnonymousObjectCaptureSource, CompoundAssignmentTarget,
    SourceConstructorMatcher,
};
/// Types carried by the public source-set analysis signatures, re-exported here so process
/// adapters do not have to reach through the frontend boundary into source classification.
pub use crate::source::{SourceInput, SourceKind};

/// Analysis result for a jointly compiled source set.
///
/// Inspection-oriented entry points retain `files`/`types`. Emission-oriented analysis deliberately
/// returns those vectors empty after inline FIR preparation; Pass 2 reparses through
/// `reparse_sources` and never observes the Pass-1 arenas.
pub struct SourceSetAnalysis {
    pub files: Vec<File>,
    pub symbols: FrontendSymbols,
    pub types: Vec<Option<FrontendTypeInfo>>,
    parse_errors: Vec<bool>,
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
            parse_errors,
            reparse_sources,
            streamed,
        } = analysis;
        drop(files);
        drop(types);
        drop(parse_errors);
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
        let mut local_name_counters = LocalNameCounters::default();
        crate::parser::visit_declaration_units_with_features(
            &self.text,
            &tokens,
            diags,
            &self.features,
            |mut file, diags| {
                file.is_common = self.is_common;
                record_local_class_name_provenance_with_counters(
                    &mut file,
                    &mut local_name_counters,
                );
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
}

fn diagnostic_streamed_state(
    mut index: crate::fir::ResolvedModuleIndex,
    sources: crate::fir::SourceMap,
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

/// Publish expect-owned default presence on surviving actual headers and return the stable
/// provider→target work. Matched expect syntax remains in the active Pass-1 parser stream only
/// until those defaults become checked FIR; compact-header exclusion keeps it out of signatures.
struct ActualizedHeaders {
    /// Top-level `expect` declarations an `actual` replaced. Their subtrees are excluded.
    matched: std::collections::HashSet<crate::fir::DeclarationId>,
    /// The `actual` declarations that actualized something.
    targets: std::collections::HashSet<crate::fir::DeclarationId>,
    /// Expect-owned defaults, as stable provider→target work.
    defaults: Vec<crate::fir::DefaultArgumentProvider>,
    /// Implementations that did not write `actual`.
    unmarked: Vec<crate::fir::DeclarationId>,
    /// `expect` declarations an implementation was written for and none matched.
    incompatible: std::collections::HashSet<crate::fir::DeclarationId>,
    /// Matched classifiers and the `expect` members they never implemented.
    unactualized_members: Vec<crate::fir::UnactualizedMembers>,
    /// `actual` members rejected only for spelling their type parameters differently.
    incompatible_members: std::collections::HashSet<crate::fir::DeclarationId>,
}

fn actualize_headers_and_collect_inherited_defaults(
    headers: &mut crate::fir::StreamedHeaderModule,
    actualization: crate::fir::Actualization,
) -> ActualizedHeaders {
    let crate::fir::Actualization {
        pairs,
        unmarked,
        incompatible,
        unactualized_members,
        incompatible_members,
    } = actualization;
    let matched = pairs
        .iter()
        .filter_map(|pair| {
            headers
                .declarations
                .anchor(pair.expect)
                .is_some_and(|anchor| anchor.owner.is_none())
                .then_some(pair.expect)
        })
        .collect();
    // The declarations that actualized something, which is the authority on whether an `actual`
    // found its `expect`: this matcher compares resolved type SHAPES and follows an
    // `actual typealias`, so it pairs `expect val S.tag: S` with `actual val String.tag: String`
    // where a name-and-arity key cannot.
    let actualized_targets = pairs.iter().map(|pair| pair.actual).collect();
    let inherited_defaults = pairs
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
    ActualizedHeaders {
        matched,
        targets: actualized_targets,
        defaults: work,
        unmarked,
        incompatible,
        unactualized_members,
        incompatible_members,
    }
}

/// Reject every top-level expect subtree for which compact actualization found no platform root.
/// This is a source-set semantic check, not a consequence of whether a particular expect spelling
/// happens to have an executable body. Reporting it before exclusion also prevents body checking
/// or a backend from accidentally treating a body-less expect function as an abstract declaration.
///
/// `silent` suppresses the DIAGNOSTIC only, never the source rejection: once any `expect`
/// declaration carries a body the reference compiler never reaches actualization, so it says
/// nothing about a missing `actual` anywhere in the compilation — but the sources still must not
/// reach a backend that would read a body-less header as abstract.
#[allow(clippy::too_many_arguments)]
fn report_unmatched_expect_roots(
    headers: &crate::fir::StreamedHeaderModule,
    matched: &std::collections::HashSet<crate::fir::DeclarationId>,
    incompatible: &std::collections::HashSet<crate::fir::DeclarationId>,
    symbols: &FrontendSymbols,
    module_name: &str,
    silent: bool,
    rejected_sources: &mut [bool],
    diags: &mut DiagSink,
) {
    let mut unmatched = headers
        .stubs
        .iter()
        .filter(|stub| {
            stub.flags.has(crate::fir::DeclarationFlags::EXPECT)
            && headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner.is_none())
            && !matched.contains(&stub.id)
            // An implementation WAS written for this header and its input shapes disagree. The
            // reference compiler reports that on the implementation and says nothing here; naming
            // the header as unactualized as well reports one mismatch twice, from the side that
            // did not get it wrong.
            && !incompatible.contains(&stub.id)
            && !symbols.is_source_optional_expectation(stub.id)
        })
        .collect::<Vec<_>>();
    // The reference compiler reports a missing `actual` while actualizing IR, which matches every
    // top-level expect classifier of the module before it links any callable. So its ledger names
    // each unactualized classifier, in source order, before any unactualized function or property.
    unmatched.sort_by_key(|stub| stub.kind != crate::fir::DeclarationKind::Classifier);
    for stub in unmatched {
        let source = stub.source.raw() as usize;
        if let Some(rejected) = rejected_sources.get_mut(source) {
            *rejected = true;
        }
        if silent {
            continue;
        }
        diags.set_file(stub.source.raw());
        let name = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
            .unwrap_or("<anonymous>");
        // The reference compiler points at the `expect` KEYWORD, not at the declaration it
        // precedes. The header module recorded that keyword beside the flag that makes this stub an
        // expect at all, so there is nothing to search for and nothing to substitute: a stub
        // flagged `EXPECT` without one is a broken header product, and saying so is the only honest
        // answer — relocating the message to the declaration hides which position is wrong.
        let Some(&range) = headers.expect_keywords.get(&stub.id) else {
            diags.error(
                stub.range,
                format!(
                    "internal error: expect declaration {name} reached actualization with no \
                     recorded `expect` keyword"
                ),
            );
            continue;
        };
        // The target is the PLATFORM's name for itself. A constant here would still say `for JVM`
        // under another backend, so a provider that does not name itself is a broken contract
        // rather than an invitation to pick one.
        let Some(target) = symbols.libraries.diagnostic_target_name() else {
            diags.error(
                range,
                format!(
                    "internal error: the semantic platform did not name itself, so \
                     {name} cannot be reported as unactualized"
                ),
            );
            continue;
        };
        diags.error_kind(
            range,
            crate::diag::DiagnosticKind::Actualization,
            crate::diagnostic_wording::no_actual_for_expect(name, module_name, target),
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
    let overrides = classifiers
        .iter()
        .flat_map(|classifier| index.function_overrides(*classifier))
        .cloned()
        .collect::<Vec<_>>();
    loop {
        let providers = work
            .iter()
            .map(|item| (item.target, item.provider))
            .collect::<std::collections::HashMap<_, _>>();
        let mut additions = Vec::new();
        for edge in &overrides {
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
    let mut file = parse_source(src, &features, diags);
    record_local_class_name_provenance(&mut file);
    file
}

/// Analyze a source set with project-wide and per-source language features.
pub fn analyze_source_set_with_features(
    sources: &[SourceInput<'_>],
    platform: impl Into<PlatformProvider>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> SourceSetAnalysis {
    analyze_source_set_impl(
        sources,
        sources.len(),
        sources.len(),
        platform.into(),
        project_features,
        DEFAULT_MODULE_NAME,
        |_, _| {},
        diags,
        false,
        true,
    )
}

/// What a source set is analyzed against: the semantic platform and the native compiler plugins the
/// compilation runs.
///
/// The platform is either fully constructed or a terminal initialization diagnostic. The failed
/// state does not implement symbol lookup and therefore cannot leak dependency corruption as
/// ordinary absence before the frontend reports it.
///
/// No native plugin runs unless the driver selects one with [`PlatformProvider::with_native_plugins`]
/// (the CLI from kotlinc's `-Xplugin`/`-P` switches): kotlinc synthesizes nothing for a plugin it was
/// not given, so a `@Serializable` class gets no serializer without the serialization plugin.
pub struct PlatformProvider {
    platform: Result<Box<dyn SemanticPlatform>, crate::libraries::PlatformInitializationError>,
    native_plugins: NativePlugins,
}

impl PlatformProvider {
    /// Run `native_plugins` in this analysis and in the emission that consumes it.
    pub fn with_native_plugins(self, native_plugins: NativePlugins) -> Self {
        Self {
            native_plugins,
            ..self
        }
    }
}

impl From<Box<dyn SemanticPlatform>> for PlatformProvider {
    fn from(platform: Box<dyn SemanticPlatform>) -> Self {
        Self {
            platform: Ok(platform),
            native_plugins: NativePlugins::none(),
        }
    }
}

impl<T> From<Box<T>> for PlatformProvider
where
    T: SemanticPlatform + 'static,
{
    fn from(platform: Box<T>) -> Self {
        Self::from(platform as Box<dyn SemanticPlatform>)
    }
}

impl<T> From<Result<T, crate::libraries::PlatformInitializationError>> for PlatformProvider
where
    T: SemanticPlatform + 'static,
{
    fn from(platform: Result<T, crate::libraries::PlatformInitializationError>) -> Self {
        Self {
            platform: platform.map(|platform| Box::new(platform) as Box<dyn SemanticPlatform>),
            native_plugins: NativePlugins::none(),
        }
    }
}

/// Analyze a source set with checked, inferred, and declaration-only file prefixes.
pub fn analyze_source_set_prefix_with_features(
    sources: &[SourceInput<'_>],
    checked_count: usize,
    inferred_count: usize,
    platform: impl Into<PlatformProvider>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> SourceSetAnalysis {
    analyze_source_set_impl(
        sources,
        checked_count,
        inferred_count,
        platform.into(),
        project_features,
        DEFAULT_MODULE_NAME,
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
    platform: impl Into<PlatformProvider>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> SourceSetAnalysis {
    analyze_source_set_impl(
        sources,
        checked_count,
        inferred_count,
        platform.into(),
        project_features,
        DEFAULT_MODULE_NAME,
        |_, _| {},
        diags,
        true,
        true,
    )
}

pub fn analyze_source_set_with_features_and_prepare<F, P>(
    sources: &[SourceInput<'_>],
    platform: P,
    project_features: &LangFeatures,
    prepare_symbols: F,
    diags: &mut DiagSink,
) -> SourceSetAnalysis
where
    F: FnOnce(&[File], &mut FrontendSymbols),
    P: Into<PlatformProvider>,
{
    analyze_source_set_with_features_and_prepare_prefix(
        sources,
        sources.len(),
        sources.len(),
        platform.into(),
        project_features,
        prepare_symbols,
        diags,
    )
}

/// Analyze a source set for the production two-pass pipeline without allowing a target to mutate
/// frontend semantic state. Target layout is realized only after checked common IR exists; file
/// containers, physical names, and descriptors therefore cannot influence Pass-1 signature
/// solving or Pass-2 body checking.
/// kotlinc's default `-module-name`, and what a diagnostic naming the module says when the caller
/// states none.
pub const DEFAULT_MODULE_NAME: &str = "main";

/// [`analyze_source_set_streaming_with_features`] for a caller that knows its `-module-name`. The
/// name reaches only diagnostics that spell it, never resolution.
pub fn analyze_source_set_streaming_with_module(
    sources: &[SourceInput<'_>],
    platform: impl Into<PlatformProvider>,
    project_features: &LangFeatures,
    module_name: &str,
    diags: &mut DiagSink,
) -> StreamingSourceSetAnalysis {
    analyze_source_set_impl(
        sources,
        sources.len(),
        sources.len(),
        platform.into(),
        project_features,
        module_name,
        |_, _| {},
        diags,
        false,
        false,
    )
    .into()
}

pub fn analyze_source_set_streaming_with_features(
    sources: &[SourceInput<'_>],
    platform: impl Into<PlatformProvider>,
    project_features: &LangFeatures,
    diags: &mut DiagSink,
) -> StreamingSourceSetAnalysis {
    analyze_source_set_impl(
        sources,
        sources.len(),
        sources.len(),
        platform.into(),
        project_features,
        DEFAULT_MODULE_NAME,
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
    platform: PlatformProvider,
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
        DEFAULT_MODULE_NAME,
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
    platform: PlatformProvider,
    project_features: &LangFeatures,
    module_name: &str,
    prepare_symbols: F,
    diags: &mut DiagSink,
    trim_support_bodies: bool,
    retain_inspection_analysis: bool,
) -> SourceSetAnalysis
where
    F: FnOnce(&[File], &mut FrontendSymbols),
{
    let diagnostics_start = diags.diags.len();
    let mut files = Vec::with_capacity(sources.len());
    let mut parse_errors = Vec::with_capacity(sources.len());
    // Set when any file writes an `expect` declaration with an implementation. The reference
    // compiler stops before actualization once one exists, so the unmatched-expect report below is
    // skipped for the WHOLE compilation rather than per file.
    let mut expect_bodies_rejected = false;
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
            features: features.clone(),
            #[cfg(test)]
            parse_count: std::cell::Cell::new(0),
            #[cfg(test)]
            released_before_collection: false,
        });
        let diagnostics_before = diags.diags.len();
        let mut file = parse_source_kind(source.text, source.kind, &features, diags);
        file.is_common = source.is_common;
        if source.kind != SourceKind::Java {
            record_local_class_name_provenance(&mut file);
        }
        if source.kind == SourceKind::Kotlin {
            header_validation::validate(&file, diags);
            // `expect`/`actual` outside a multiplatform project is an ERROR, not a no-op. Accepting
            // it emitted an artifact that could not link: a call to an unmatched `expect fun` was
            // written as an `invokestatic` of a method the facade does not declare, so the program
            // failed at its first call rather than at compile time. Reported once per modifier, at
            // the modifier, in the reference compiler's own words.
            if !multiplatform {
                // The wording does not vary with which modifier was written — an `actual` reports
                // the same sentence, naming both — and a member `actual` is reported too, at its
                // own column. Both measured against the reference compiler rather than assumed.
                for (_, span) in &file.multiplatform_modifiers {
                    diags.error(
                        *span,
                        "'expect' and 'actual' declarations can be used only in multiplatform \
                         projects. Learn more about Kotlin Multiplatform: \
                         https://kotl.in/multiplatform-setup",
                    );
                }
            }
            // An `expect` declaration that carries an implementation is an error on its own, with
            // or without the feature — the reference compiler reports both sentences for a file
            // that has neither, in this order. Once any such body exists it stops before
            // actualization, so a body error suppresses the unmatched-expect report for the whole
            // compilation, not just for this file (measured with two files: a body error in one
            // silenced a clean unmatched `expect` in the other).
            expect_bodies_rejected |= header_validation::validate_expect_bodies(&file, diags);
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
        if let Some((source, stubs, stable_by_transient)) = extracted {
            source_contracts.extend(crate::resolve::extract_source_contract_candidates(
                &file, source, &stubs,
            ));
            // Inferred signatures are structurally copied into the compact graph before ordinary
            // signature collection enters its own grown stack. Keep this recursive extraction on
            // the same depth-safe boundary: the public indexed analysis path must preserve the
            // checker's documented nesting contract without relying on the removed direct
            // collection entry point.
            crate::wide_stack::on_wide_stack(|| {
                signature_constraints.extract_file(&file, source, &stubs, |span| {
                    pass1_builder.source_origin(source, span)
                });
            });
            // Compact signature extraction has consumed every ordinary expression dependency for
            // this source. Production keeps the parser body arenas only for bounded Pass-1 work
            // that has not moved to its own store yet: inline checking, const evaluation, and MPP
            // actualization matching. Inspection entry points retain their parser-keyed view for
            // editor queries while consuming declaration identities from the finalized index.
            let needs_bounded_pass_one_syntax = multiplatform
                || has_signature_defaults(&file)
                || retained_syntax::has_classifier_annotation_arguments(&file)
                || stubs.iter().any(|stub| {
                    stub.flags.has(crate::fir::DeclarationFlags::INLINE)
                        || stub.flags.has(crate::fir::DeclarationFlags::CONST)
                });
            local_class_contexts.push(crate::resolve::pass_one_local_class_context(
                &file,
                &stubs,
                &stable_by_transient,
            ));
            if !retain_inspection_analysis && index < inferred_count && !multiplatform {
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
            // A declaration-only support source intentionally contributes no compact header
            // inventory. Its transient local classifiers are body-owned and likewise publish no
            // Pass-1 context; later phases must not fabricate stable identities for declarations
            // excluded at this boundary.
            local_class_contexts.push(crate::resolve::PassOneLocalClassContext::default());
        }
        files.push(file);
    }

    // Which `actual` declarations actualize nothing is a question about the SOURCE SET, so it is
    // answered here, while every file's syntax is still live and before Pass-1 compaction. The
    // diagnostic itself names the declaration as the reference compiler's renderer does, so it is
    // reported once resolution has published the types it renders.
    assert!(checked_count <= inferred_count && inferred_count <= files.len());
    let mut pass1_headers = pass1_builder.finish();
    // Answered here, while every file's syntax is still live and the compact inventory that
    // interned it is already built: each `actual` is paired with its stable identity at the moment
    // its syntax is copied, so identity and coordinate travel together from this point on.
    let unmatched_actuals = if multiplatform {
        no_expect_for_actual::collect(&files, &pass1_headers)
    } else {
        Vec::new()
    };
    // Read in the same breath and for the same reason: a classifier that implements none of its
    // expectation's members lists what it owes, and those are `expect` declarations whose syntax
    // is live only here.
    let expected_members = if multiplatform {
        no_expect_for_actual::expected_members(&files, &pass1_headers)
    } else {
        std::collections::HashMap::new()
    };
    let source_classifiers = pass1_headers.source_classifier_names();
    let platform_sources = sources
        .iter()
        .enumerate()
        .filter(|(_, source)| source.kind == SourceKind::Java)
        .map(
            |(source, input)| crate::libraries::PlatformSourceHeaderInput {
                source,
                text: input.text,
                file_stem: input.file_stem,
            },
        )
        .collect::<Vec<_>>();
    let PlatformProvider {
        platform,
        native_plugins,
    } = platform;
    let platform = match platform {
        Ok(platform) => platform,
        Err(error) => {
            diags.set_file(0);
            diags.error(Span::new(0, 0), error.message);
            diags.collapse_duplicates_from(diagnostics_start);
            let types = files.iter().map(|_| None).collect();
            return SourceSetAnalysis {
                files: if retain_inspection_analysis {
                    files
                } else {
                    Vec::new()
                },
                symbols: FrontendSymbols::default(),
                types,
                parse_errors,
                reparse_sources,
                streamed: None,
            };
        }
    };
    if let Err(error) = platform.validate_initialization() {
        diags.set_file(0);
        diags.error(Span::new(0, 0), error.message);
        diags.collapse_duplicates_from(diagnostics_start);
        let types = files.iter().map(|_| None).collect();
        return SourceSetAnalysis {
            files: if retain_inspection_analysis {
                files
            } else {
                Vec::new()
            },
            symbols: FrontendSymbols::default(),
            types,
            parse_errors,
            reparse_sources,
            streamed: None,
        };
    }
    if let Err(error) =
        platform.install_source_module_headers(&platform_sources, &source_classifiers)
    {
        diags.set_file(error.source as u32);
        diags.error(Span::new(0, 0), error.message);
    }
    let (
        mut signature_default_work_items,
        matched_expect_declarations,
        actualized_targets,
        incompatible_expects,
        unactualized_members,
        incompatible_members,
    ) = if multiplatform {
        let bindings =
            crate::resolve::actualization_type_bindings(&pass1_headers, platform.as_ref());
        let actualization = crate::fir::actualization(&pass1_headers, &bindings);
        let ActualizedHeaders {
            matched,
            targets: actualized_targets,
            defaults,
            unmarked,
            incompatible,
            unactualized_members,
            incompatible_members,
        } = actualize_headers_and_collect_inherited_defaults(&mut pass1_headers, actualization);
        // Reported here, while every file's syntax is still live: the diagnostic points at the
        // declaration's NAME, and the compact inventory anchors only its whole range.
        no_expect_for_actual::report_unmarked_implementations(
            &unmarked,
            &files,
            &pass1_headers,
            diags,
        );
        pass1_headers.exclude_declaration_subtrees(&matched);
        // Explicit expect→actual default mappings remain valid after exclusion because their
        // provider anchors and bounded syntax live through the rest of Pass 1. Enumerate ordinary
        // self-owned defaults only after exclusion so a removed expect constructor cannot schedule
        // an orphan target with no surviving signature or callable.
        let signature_default_work_items = signature_default_work(&pass1_headers, &defaults);
        // Actualization publishes stable expect-default providers before syntax is compacted. Once
        // that source-set operation is complete, retain only Pass-1 signature/inline fragments.
        if !retain_inspection_analysis {
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
        (
            signature_default_work_items,
            matched,
            actualized_targets,
            incompatible,
            unactualized_members,
            incompatible_members,
        )
    } else {
        (
            signature_default_work(&pass1_headers, &[]),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            Vec::new(),
            std::collections::HashSet::new(),
        )
    };
    let platform = if inferred_count < files.len() {
        let mut dependency_diags = DiagSink::new();
        let mut dependency_symbols = crate::resolve::collect_signatures_with_cp_and_plugins(
            &files[inferred_count..],
            platform,
            native_plugins.clone(),
            &mut dependency_diags,
        );
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
        native_plugins,
        diags,
    );
    if multiplatform {
        report_unmatched_expect_roots(
            &pass1_headers,
            &matched_expect_declarations,
            &incompatible_expects,
            &symbols,
            module_name,
            expect_bodies_rejected,
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
    let retained_anonymous_captures = if retain_inspection_analysis {
        Some(crate::resolve::discover_anonymous_object_captures(
            &files[..inferred_end],
            &mut symbols,
        ))
    } else if has_inline_capture_roots {
        crate::resolve::discover_inline_anonymous_object_captures(
            &files[..inferred_end],
            &inline_capture_selection.roots[..inferred_end],
            &inline_capture_selection.bodies[..inferred_end],
            &mut symbols,
        );
        None
    } else {
        None
    };
    if retain_inspection_analysis || has_inline_capture_roots {
        crate::resolve::install_streamed_anonymous_capture_declarations(
            &files[..inferred_end],
            &mut pass1_headers,
            &mut symbols,
        );
    }
    // Solving an inferred signature graph recursively evaluates its compact expression tree. It
    // has the same depth contract as extraction and body checking, so enter it on the shared grown
    // stack rather than inheriting the caller thread's incidental stack size.
    let streamed_index = crate::wide_stack::on_wide_stack(|| {
        crate::resolve::finalized_streamed_signature_index(
            &pass1_headers,
            &mut symbols,
            signature_constraints,
            source_contracts,
            diags,
        )
    });
    let mut recovery_streamed = None;
    let pending_streamed = if streamed_index.failures.is_empty() {
        let mut index = streamed_index.index;
        // Finalized signatures and their stable declaration ancestry form one Pass-1 product.
        // Publish the inventory before deriving declaration-owned metadata such as enum-entry
        // override edges; those entries deliberately have no ordinary classifier header.
        pass1_headers.publish_declaration_inventory(&mut index);
        crate::resolve::project_finalized_signatures(&index, &mut symbols);
        crate::resolve::publish_checked_classifier_annotations(
            &files[..inferred_end],
            &index,
            &mut symbols,
            diags,
        );
        crate::resolve::finalize_streamed_top_level_conflicts(&pass1_headers, &mut symbols, diags);
        // An `actual` that actualizes nothing is named by the reference compiler's declaration
        // renderer over its RESOLVED signature, so it is reported only once finalization has
        // published one: an inferred return (`actual fun f() = 1`) is `<not determined>` before
        // this point. The declarations themselves were selected while every file's syntax was
        // still live.
        if !expect_bodies_rejected {
            no_expect_for_actual::report(
                &unmatched_actuals,
                &actualized_targets,
                &incompatible_members,
                &symbols,
                &pass1_headers,
                diags,
            );
            no_expect_for_actual::report_unactualized_members(
                &unactualized_members,
                &unmatched_actuals,
                &expected_members,
                &symbols,
                &pass1_headers,
                diags,
            );
        }
        // A `const val` initializer is a stable declaration dependency. Check each such bounded
        // fragment now, while Pass 1 still owns its AST and exact operator selections can be
        // consumed; retain only the folded payload before the signature graph and arenas die.
        crate::resolve::publish_checked_compile_time_constants(
            &files[..inferred_end],
            &mut symbols,
        );
        crate::resolve::publish_stable_declaration_metadata(&mut index, &symbols);
        crate::resolve::publish_override_plans(&mut index, &symbols);
        crate::resolve::function_type_parameters::publish_function_type_parameters(
            &mut index, &symbols,
        );
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
            &local_class_contexts[..inferred_end],
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
                recovery_streamed = Some(diagnostic_streamed_state(index, sources));
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
        // The declarations that DID finalize are still the module's facts. Project them into the
        // source symbol environment consumed beside the partial stable index; otherwise one
        // genuine signature error would make every use of an unrelated inferred declaration look
        // unresolved in an editor's steady state. Failed declarations remain absent.
        crate::resolve::project_finalized_signatures(&index, &mut symbols);
        let (index, sources, _) = pass1_headers.finish(index);
        recovery_streamed = Some(diagnostic_streamed_state(index, sources));
        None
    };
    if !retain_inspection_analysis {
        // Typed classifier annotations are the final consumers of declaration-only legacy `File`
        // views. Once folded, retain a parser fragment only when it still owns executable syntax
        // that Pass 1 must turn into checked FIR (inline/default/const work).
        for file in files.iter_mut().take(inferred_end) {
            if file.expr_arena.is_empty() && file.stmt_arena.is_empty() {
                *file = File::default();
            }
        }
    }
    if trim_support_bodies {
        for file in &mut files[checked_count.min(inferred_end)..inferred_end] {
            file.release_body_arenas();
        }
    }
    let (types, streamed) = if retain_inspection_analysis {
        // Successful finalization and diagnostic recovery both publish the same stable declaration
        // inventory. Recovery merely lacks signatures for declarations that failed to finalize;
        // it must not reopen the parser-keyed checker and silently switch identity models.
        let index = pending_streamed
            .as_ref()
            .map(|(module, _, _)| module.index())
            .or_else(|| {
                recovery_streamed
                    .as_ref()
                    .map(|streamed| streamed.module.index())
            })
            .expect("retained analysis must publish a stable declaration index");
        let types = check_source_set_skipping_with_index(
            &files,
            &mut symbols,
            index,
            retained_anonymous_captures
                .as_deref()
                .expect("retained analysis must preserve capture discovery"),
            &parse_errors,
            checked_count,
            diags,
        );
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
                &local_class_contexts,
                &parse_errors,
                checked_count,
                &mut symbols,
                diags,
            )
        });
        (Vec::new(), streamed)
    };
    let streamed = streamed.or(recovery_streamed);
    if let Err(error) = symbols.libraries.validate_initialization() {
        diags.diags.truncate(diagnostics_start);
        diags.set_file(0);
        diags.error(Span::new(0, 0), error.message);
    }
    diags.collapse_duplicates_from(diagnostics_start);
    let analysis = SourceSetAnalysis {
        files: if retain_inspection_analysis {
            files
        } else {
            Vec::new()
        },
        symbols,
        types,
        parse_errors,
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

fn check_source_set_skipping_with_index(
    files: &[File],
    symbols: &mut FrontendSymbols,
    index: &crate::fir::ResolvedModuleIndex,
    anonymous_captures: &[std::collections::HashMap<
        crate::ast::DeclId,
        Vec<crate::resolve::AnonymousObjectCapture>,
    >],
    skip: &[bool],
    checked_count: usize,
    diags: &mut DiagSink,
) -> Vec<Option<FrontendTypeInfo>> {
    let source_declarations = std::sync::Arc::new(
        crate::resolve::inspection_source_declaration_keys(files, index),
    );
    files
        .iter()
        .enumerate()
        .map(|(source, _)| {
            if source >= checked_count || skip.get(source).copied().unwrap_or(false) {
                return None;
            }
            diags.set_file(source as u32);
            Some(
                crate::resolve::check_preinferred_file_in_source_set_with_index(
                    files,
                    source as u32,
                    symbols,
                    index,
                    source_declarations.clone(),
                    anonymous_captures.get(source),
                    diags,
                ),
            )
        })
        .collect()
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
    let mut analysis = analyze_source_set(&[src], platform, diags);
    let file = analysis.files.pop().unwrap_or_default();
    let parsed = !analysis.parse_errors.pop().unwrap_or(true);
    let signatures_finalized = analysis
        .streamed
        .as_ref()
        .is_some_and(|streamed| !streamed.diagnostic_recovery);
    let info = (parsed && signatures_finalized)
        .then(|| analysis.types.pop().flatten())
        .flatten();
    (file, parsed.then_some(analysis.symbols), info)
}

/// Parse and check a source with no external libraries.
pub fn analyze_source_standalone(
    src: &str,
    diags: &mut DiagSink,
) -> (File, Option<FrontendSymbols>, Option<FrontendTypeInfo>) {
    analyze_source(src, Box::new(EmptySymbolSource), diags)
}

/// Record local-class source ownership and ordering without choosing a target spelling.
pub fn record_local_class_name_provenance(file: &mut crate::ast::File) {
    let mut counters = LocalNameCounters::default();
    record_local_class_name_provenance_with_counters(file, &mut counters);
}

/// The naming sequences of one source file, carried across its declaration units: kotlinc's
/// local-class names and its lifted local-callable names each number one sequence per file.
#[derive(Default)]
struct LocalNameCounters {
    classes: std::collections::HashMap<Vec<String>, u32>,
    lifted: local_function_names::LiftingCounters,
}

fn record_local_class_name_provenance_with_counters(
    file: &mut crate::ast::File,
    counters: &mut LocalNameCounters,
) {
    let lifted = local_function_names::record(file, &mut counters.lifted);
    file.lambda_lifting_sites.extend(lifted.lambdas);
    file.local_function_lifting_sites
        .extend(lifted.local_functions);
    file.local_delegate_lifting_sites
        .extend(lifted.local_delegates);
    let invented = local_class_names::invent(file, &mut counters.classes);
    file.local_class_name_provenance.extend(invented.classes);
    file.anonymous_object_enclosing_functions
        .extend(invented.anonymous_enclosing_functions);
    file.suspend_continuation_ordinals
        .extend(invented.continuations);
    file.callable_reference_provenance.extend(
        invented
            .references
            .into_iter()
            .map(|(expression, provenance)| (expression.0, provenance)),
    );
}

#[cfg(test)]
mod tests;
