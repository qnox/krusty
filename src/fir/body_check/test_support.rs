use crate::ast::{Decl, FunBody};
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::{EmptySymbolSource, SemanticPlatform};
use crate::source::SourceInput;

use super::*;

pub(super) fn checked_function_body(
    source: &str,
    function_name: &str,
) -> (FirBody, ResolvedModuleIndex) {
    checked_function_body_with_platform(source, function_name, Box::new(EmptySymbolSource))
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

fn checked_function_body_with_platform_and_features(
    source: &str,
    function_name: &str,
    platform: Box<dyn SemanticPlatform>,
    features: &LangFeatures,
) -> (FirBody, ResolvedModuleIndex) {
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("FirBody")],
        platform,
        features,
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let (mut index, _, _, mut sources) = streamed.module.into_parts();
    let file = &analysis.files[0];
    crate::resolve::publish_checked_local_signatures(
        file,
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        analysis.types[0].as_ref().expect("checked file"),
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let active =
        ActiveSourceDeclarations::bind_complete_source(file, SourceFileId::from_raw(0), &index)
            .expect("focused FIR tests must bind the live parser arena to stable declarations");
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
        })
        .collect::<Vec<_>>();
    let mut session = BodyCheckSession::default();
    session.install_active_source(&active);
    let body = check_body_unit_with_parameters_and_defaults(
        file,
        analysis.types[0].as_ref().expect("checked file"),
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(declaration.raw()),
        file.expr_span(root).expect("function body span"),
        Some(root),
        &parameters,
        &[],
        CheckedBodyReceiverShape {
            context_receivers: &signature.parameters[..context_count],
            context_value_count: callable.shape.context_value_count,
            extension_receiver: super::driver::body_extension_receiver(
                &index,
                declaration,
                callable.shape.extension_receiver,
            ),
        },
        None,
        matches!(function.body, FunBody::Expr(_)).then_some(signature.result),
        &index,
        sources.origins_mut(),
        &mut session,
    )
    .expect("body must build checked FIR");
    (body, index)
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
    Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            crate::toolchain::classpath_jars_for("// WITH_REFLECT"),
        )),
    ))
}

pub(super) fn jvm_stdlib_semantics() -> Box<dyn SemanticPlatform> {
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB\n// WITH_REFLECT");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
    ))
}
