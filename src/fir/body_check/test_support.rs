use crate::ast::{Decl, FunBody};
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::{EmptySymbolSource, SemanticPlatform};
use crate::source::SourceInput;

use super::*;

struct SingletonExtensionSource {
    callable: crate::libraries::FunctionInfo,
}

impl crate::symbol_source::SymbolSource for SingletonExtensionSource {
    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        let matches = namespace
            == crate::symbol_source::SymbolNamespace::Package(crate::types::TypeName::ROOT)
            && name == "read";
        std::rc::Rc::new(crate::libraries::ResolvedSymbols {
            builtin_classifier: false,
            classifier_name: None,
            classifier_declaration: None,
            classifier: None,
            callables: if matches {
                crate::libraries::Callables::Functions(crate::libraries::FunctionSet {
                    overloads: vec![self.callable.clone()],
                })
            } else {
                crate::libraries::Callables::None
            },
            importable_declaration: matches,
        })
    }
}

impl SemanticPlatform for SingletonExtensionSource {}

pub(super) fn singleton_extension_source() -> Box<dyn SemanticPlatform> {
    let singleton = Ty::obj("Token");
    let mut callable = crate::libraries::LibraryCallable::library(
        "dependency/ExtensionsKt",
        "read",
        vec![singleton],
        Ty::Int,
        Ty::Int,
        "",
    );
    callable.external_identity = Some(crate::fir::ExternalCallableId::from_raw(702));
    Box::new(SingletonExtensionSource {
        callable: crate::libraries::FunctionInfo::plain(
            crate::libraries::FnKind::Extension,
            Some(singleton),
            callable,
        ),
    })
}

pub(super) fn root_external_function_source() -> Box<dyn SemanticPlatform> {
    let mut callable =
        crate::libraries::LibraryCallable::library("", "read", vec![Ty::Int], Ty::Int, Ty::Int, "");
    callable.external_identity = Some(crate::fir::ExternalCallableId::from_raw(703));
    Box::new(SingletonExtensionSource {
        callable: crate::libraries::FunctionInfo::plain(
            crate::libraries::FnKind::TopLevel,
            None,
            callable,
        ),
    })
}

/// A complete live source checked with production Pass-2 facts before a focused FIR test mutates it.
pub(super) struct PreparedFirSource {
    pub(super) file: crate::ast::File,
    pub(super) info: crate::resolve::TypeInfo,
    pub(super) index: ResolvedModuleIndex,
    pub(super) active: ActiveSourceDeclarations,
    pub(super) source: SourceFileId,
    pub(super) ordinary: Vec<crate::fir::BodyWorkItem>,
    pub(super) inline_bodies: crate::fir::InlineBodyStore,
    pub(super) sources: crate::fir::SourceMap,
}

pub(super) fn prepare_fir_source(
    inputs: &[SourceInput<'_>],
    source_index: usize,
    platform: Box<dyn SemanticPlatform>,
    features: &LangFeatures,
) -> PreparedFirSource {
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        inputs,
        crate::frontend::PlatformProvider::jvm(platform),
        features,
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let source = SourceFileId::from_raw(
        u32::try_from(source_index).expect("focused FIR source index exceeds u32"),
    );
    let file = &analysis.files[source_index];
    let ordinary = streamed.ordinary_body_work(file, source);
    let (mut index, inline_bodies, _defaults, sources) = streamed.module.into_parts();
    let active = ActiveSourceDeclarations::bind_complete_source(file, source, &index)
        .expect("focused Pass 2 source binding");
    let roots = file
        .decls
        .iter()
        .map(|declaration| {
            active
                .file_declaration(file, *declaration)
                .expect("live file declaration root")
        })
        .collect::<Vec<_>>();
    let selected_roots = roots
        .iter()
        .map(|root| active.span(file, *root).expect("selected root source span"))
        .collect::<crate::fx_hash::HashSet<_>>();
    let selected_stable_bodies = ordinary
        .iter()
        .map(|work| work.declaration)
        .chain(active.stable_declarations().filter(|declaration| {
            active
                .function(file, *declaration)
                .is_some_and(|function| !matches!(function.body, FunBody::None))
        }))
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut selected_body_spans = selected_stable_bodies
        .iter()
        .filter_map(|declaration| active.span(file, *declaration))
        .collect::<crate::fx_hash::HashSet<_>>();
    for &body in &selected_stable_bodies {
        let mut current = body;
        while let Some(owner) = index
            .declaration_header(current)
            .and_then(|header| header.owner)
            .or_else(|| {
                index
                    .declaration_anchor(current)
                    .and_then(|anchor| anchor.owner)
            })
        {
            if roots.contains(&owner) {
                if !index
                    .declaration_header(owner)
                    .is_some_and(|header| header.kind == DeclarationKind::Classifier)
                {
                    if let Some(span) = active.span(file, owner) {
                        selected_body_spans.insert(span);
                    }
                }
                break;
            }
            if let Some(span) = active.span(file, owner) {
                selected_body_spans.insert(span);
            }
            current = owner;
        }
    }
    let mut pass_two_symbols = std::mem::take(&mut analysis.symbols).into_pass_two_symbols();
    let cache = crate::fir::StreamedModuleProjectionCache::default();
    let info = crate::resolve::check_selected_declarations_in_pass_two(
        file,
        source.raw(),
        &selected_roots,
        &selected_body_spans,
        &active,
        &selected_stable_bodies,
        &mut pass_two_symbols,
        &index,
        &cache,
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    for root in roots {
        crate::resolve::publish_checked_local_signatures_in_pass_two_root(
            file,
            &active,
            source,
            pass_two_symbols.semantic_platform(),
            &info,
            &mut index,
            root,
            &selected_stable_bodies,
        )
        .expect("fresh Pass 2 local signatures");
    }
    PreparedFirSource {
        file: analysis.files.remove(source_index),
        info,
        index,
        active,
        source,
        ordinary,
        inline_bodies,
        sources,
    }
}

impl PreparedFirSource {
    pub(super) fn check_all_bodies(
        &mut self,
        sink: &mut impl CheckedBodySink,
    ) -> Result<(), CheckedBodyDriverFailure> {
        let mut session = BodyCheckSession::default();
        for work in self.ordinary.iter().copied() {
            super::driver::check_and_dispatch_active_body_in_session(
                BodyCheckUnit {
                    file: &self.file,
                    info: &self.info,
                    source: self.source,
                    index: &self.index,
                },
                &self.active,
                work,
                self.sources.origins_mut(),
                &mut self.inline_bodies,
                sink,
                &mut session,
            )?;
        }
        Ok(())
    }
}

pub(super) fn checked_function_body(
    source: &str,
    function_name: &str,
) -> (FirBody, ResolvedModuleIndex) {
    checked_function_body_with_platform(source, function_name, Box::new(EmptySymbolSource))
}

pub(super) fn checked_source_set_function_body(
    sources: &[&str],
    source_index: usize,
    function_name: &str,
) -> (FirBody, ResolvedModuleIndex) {
    let inputs = sources
        .iter()
        .map(|source| SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    try_checked_source_set_function_body_rewriting(
        &inputs,
        source_index,
        function_name,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
    )
    .expect("body must build checked FIR")
}

pub(super) fn checked_function_body_with_platform(
    source: &str,
    function_name: &str,
    platform: Box<dyn SemanticPlatform>,
) -> (FirBody, ResolvedModuleIndex) {
    checked_function_body_with_platform_and_features(
        source,
        function_name,
        platform,
        &LangFeatures::new(),
    )
}

pub(super) fn checked_function_body_with_features(
    source: &str,
    function_name: &str,
    features: &LangFeatures,
) -> (FirBody, ResolvedModuleIndex) {
    checked_function_body_with_platform_and_features(
        source,
        function_name,
        Box::new(EmptySymbolSource),
        features,
    )
}

pub(super) fn checked_function_body_with_platform_and_features(
    source: &str,
    function_name: &str,
    platform: Box<dyn SemanticPlatform>,
    features: &LangFeatures,
) -> (FirBody, ResolvedModuleIndex) {
    checked_function_body_rewriting(source, function_name, platform, features, |_, _| {})
}

pub(super) fn checked_function_body_rewriting(
    source: &str,
    function_name: &str,
    platform: Box<dyn SemanticPlatform>,
    features: &LangFeatures,
    rewrite: impl FnOnce(&mut crate::ast::File, &mut crate::resolve::TypeInfo),
) -> (FirBody, ResolvedModuleIndex) {
    try_checked_function_body_rewriting(source, function_name, platform, features, rewrite)
        .expect("body must build checked FIR")
}

pub(super) fn try_checked_function_body_rewriting(
    source: &str,
    function_name: &str,
    platform: Box<dyn SemanticPlatform>,
    features: &LangFeatures,
    rewrite: impl FnOnce(&mut crate::ast::File, &mut crate::resolve::TypeInfo),
) -> Result<(FirBody, ResolvedModuleIndex), BodyCheckFailure> {
    let inputs = [SourceInput::kotlin(source).with_file_stem("FirBody")];
    try_checked_source_set_function_body_rewriting(
        &inputs,
        0,
        function_name,
        platform,
        features,
        rewrite,
    )
}

fn try_checked_source_set_function_body_rewriting(
    inputs: &[SourceInput<'_>],
    source_index: usize,
    function_name: &str,
    platform: Box<dyn SemanticPlatform>,
    features: &LangFeatures,
    rewrite: impl FnOnce(&mut crate::ast::File, &mut crate::resolve::TypeInfo),
) -> Result<(FirBody, ResolvedModuleIndex), BodyCheckFailure> {
    let mut prepared = prepare_fir_source(inputs, source_index, platform, features);
    rewrite(&mut prepared.file, &mut prepared.info);
    let file = &prepared.file;
    let index = &prepared.index;
    let active = &prepared.active;
    let source = prepared.source;
    let function = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == function_name => Some(function),
            Decl::Class(class) => class.methods.iter().find(|function| {
                function.name == function_name && !matches!(function.body, FunBody::None)
            }),
            Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("function declaration");
    let root = match function.body {
        FunBody::Expr(root) | FunBody::Block(root) => root,
        FunBody::None => panic!("function must have a body"),
    };
    let declaration = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            active
                .function(file, *declaration)
                .is_some_and(|candidate| std::ptr::eq(candidate, function))
        })
        .max_by_key(|declaration| index.declaration_header(*declaration).is_some())
        .expect("stable function declaration");
    let signature = index.signature(declaration).expect("resolved signature");
    let callable = index
        .callable_for_declaration(declaration)
        .expect("resolved callable");
    let context_count = callable.shape.context_parameter_count as usize;
    let parameters = function
        .params
        .iter()
        .zip(signature.parameters.iter().copied())
        .map(|(parameter, ty)| CheckedBodyParameter {
            name: parameter.name.as_str(),
            ty,
            span: parameter.ty.span,
            context_kind: parameter.context_kind,
            inline_modifier: crate::types::InlineParameterModifier::written(
                parameter.is_materialized_lambda,
                parameter.is_crossinline,
            ),
            name_binding: super::CheckedBodyParameterNameBinding::Lexical,
        })
        .collect::<Vec<_>>();
    let mut session = BodyCheckSession::default();
    session.install_active_source(active);
    // Member bodies consume the exact capture environments published by earlier executable
    // declarations. Enter those bodies through the ordinary source-ordered driver, just as
    // production does, before checking the requested body with its focused root/signature.
    struct PrecedingBodySink;
    impl CheckedBodySink for PrecedingBodySink {
        fn accept_finalized(&mut self, _: BodyOwnerId, _: FirBody) {}
    }
    if let Some(position) = prepared
        .ordinary
        .iter()
        .position(|work| work.declaration == declaration)
    {
        for work in prepared.ordinary[..position].iter().copied() {
            match super::driver::check_and_dispatch_active_body_in_session(
                BodyCheckUnit {
                    file,
                    info: &prepared.info,
                    source,
                    index,
                },
                active,
                work,
                prepared.sources.origins_mut(),
                &mut prepared.inline_bodies,
                &mut PrecedingBodySink,
                &mut session,
            ) {
                Ok(()) => {}
                Err(CheckedBodyDriverFailure::Check(error)) => return Err(error),
                Err(error) => panic!("preceding ordinary body setup: {error:?}"),
            }
        }
    }
    let body = check_body_unit_with_parameters_and_defaults(
        BodyCheckUnit {
            file,
            info: &prepared.info,
            source,
            index,
        },
        CheckedBodyRoot {
            owner: BodyOwnerId::from_raw(declaration.raw()),
            span: file.expr_span(root).expect("function body span"),
            expression: Some(root),
            property_storage_type: None,
            value_target: matches!(function.body, FunBody::Expr(_)).then_some(signature.result),
        },
        CheckedBodySignature {
            parameters: &parameters,
            defaults: &[],
            receiver_shape: CheckedBodyReceiverShape {
                context_receivers: &signature.parameters[..context_count],
                extension_receiver: super::driver::body_extension_receiver(
                    index,
                    declaration,
                    callable.shape.extension_receiver,
                ),
            },
        },
        prepared.sources.origins_mut(),
        &mut session,
    )?;
    Ok((body, prepared.index))
}

pub(super) fn root_expression(body: &FirBody) -> FirExprId {
    let FirStatementKind::Expression(root) = body
        .statement(body.roots()[0])
        .expect("root statement")
        .kind
    else {
        panic!("root statement must contain an expression")
    };
    root
}

pub(super) fn jvm_semantics() -> Box<dyn SemanticPlatform> {
    Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(crate::toolchain::jvm_classpath_jars_for(
                "// WITH_REFLECT",
            )),
        ))
        .expect("JVM provider initialization"),
    )
}

pub(super) fn jvm_stdlib_semantics() -> Box<dyn SemanticPlatform> {
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB\n// WITH_REFLECT");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
            crate::jvm::classpath::Classpath::new(classpath),
        ))
        .expect("JVM provider initialization"),
    )
}
