use super::*;

fn finish_pass_one(analysis: SourceSetAnalysis) -> StreamingSourceSetAnalysis {
    let analysis = StreamingSourceSetAnalysis::from(analysis);
    assert!(
        analysis.streamed.as_ref().is_some_and(|streamed| {
            !streamed.diagnostic_recovery && !streamed.module.index().retains_source_coordinates()
        }),
        "a finalized module must own compact, coordinate-free Pass-2 state"
    );
    analysis
}

fn stable_declaration_at(
    analysis: &SourceSetAnalysis,
    source: usize,
    span: Span,
    kind: crate::fir::DeclarationKind,
) -> crate::fir::DeclarationId {
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let source_id = crate::fir::SourceFileId::from_raw(source as u32);
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[source],
        source_id,
        index,
    )
    .expect("the retained test AST must bind to the stable declaration stream");
    (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| anchor.source == source_id && anchor.kind == kind)
                && active.span(&analysis.files[source], *declaration) == Some(span)
        })
        .max_by_key(|declaration| index.declaration_header(*declaration).is_some())
        .expect("stable declaration at active span")
}

#[test]
fn source_set_assigns_anonymous_identity_from_the_real_file_stem() {
    let source = "fun build(): Any = object {}";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Widget")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let declaration = *analysis.files[0]
        .anonymous_object_classes
        .values()
        .next()
        .expect("anonymous declaration");
    let crate::ast::Decl::Class(class) = analysis.files[0].decl(declaration) else {
        panic!("anonymous declaration was not a class");
    };
    assert_eq!(class.name, "WidgetKt$build$1");
    let enclosing = analysis.files[0]
        .anonymous_object_enclosing_functions
        .get(&declaration)
        .copied()
        .expect("anonymous enclosure identity");
    let crate::ast::AnonymousEnclosingFunction::TopLevel(function) = enclosing else {
        panic!("top-level build owner was not recorded exactly");
    };
    assert!(matches!(
        analysis.files[0].decl(function),
        crate::ast::Decl::Fun(function) if function.name == "build"
    ));
}

#[test]
fn anonymous_renaming_keeps_nested_classifier_ownership_coherent() {
    let source = "fun build(): Any = object { inner class Nested }";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Widget")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let classes = analysis.files[0]
        .decls
        .iter()
        .filter_map(|declaration| match analysis.files[0].decl(*declaration) {
            crate::ast::Decl::Class(class) => Some(class),
            crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => None,
        })
        .collect::<Vec<_>>();
    let nested = classes
        .iter()
        .copied()
        .find(|class| class.name.ends_with(".Nested"))
        .unwrap_or_else(|| {
            panic!(
                "nested anonymous member classifier: {:?}",
                classes
                    .iter()
                    .map(|class| (&class.name, &class.inner_of))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(nested.name, "WidgetKt$build$1.Nested");
    assert_eq!(nested.inner_of.as_deref(), Some("WidgetKt$build$1"));
    let index = analysis
        .streamed
        .as_ref()
        .expect("explicit public signature must finalize")
        .module
        .index();
    let declaration = stable_declaration_at(
        &analysis,
        0,
        nested.span,
        crate::fir::DeclarationKind::Classifier,
    );
    assert!(index
        .declaration_header(declaration)
        .expect("nested classifier header")
        .flags
        .has(crate::fir::DeclarationFlags::LOCAL_CLASS));
}

#[test]
fn emission_analysis_drops_pass_one_ast_and_type_info_before_returning() {
    let inputs = [SourceInput::kotlin(
        "inline fun increment(value: Int): Int = value + 1\n\
             fun answer(): Int = increment(41)\n",
    )
    .with_file_stem("Streaming")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some());
    assert!(
        analysis.files.is_empty(),
        "Pass-1 AST must not persist into emission"
    );
    assert!(
        analysis.types.is_empty(),
        "Pass-1 AST-keyed TypeInfo must not persist into emission"
    );
    assert_eq!(analysis.reparse_sources.len(), 1);
}

#[test]
fn emission_inline_preparation_streams_each_sources_syntax_and_checked_side_table() {
    let inputs = [
        SourceInput::kotlin(
            "package first\n\
             inline fun increment(value: Int): Int = value + 1\n\
             fun deferred(): Int = missingFirst\n",
        )
        .with_file_stem("FirstInline"),
        SourceInput::kotlin(
            "package second\n\
             inline fun decrement(value: Int): Int = value - 1\n\
             fun deferred(): Int = missingSecond\n",
        )
        .with_file_stem("SecondInline"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());
    assert!(
        analysis
            .reparse_sources
            .iter()
            .all(|source| source.parse_count() == 0),
        "Pass 1 must check retained inline syntax from the initial parse, not start a hidden source pass"
    );
    let streamed = analysis.streamed.expect("both inline files must finalize");
    assert_eq!(streamed.module.inline_bodies().len(), 2);
}

#[test]
fn emission_pass_one_checks_inline_bodies_but_defers_ordinary_body_diagnostics() {
    let inputs = [SourceInput::kotlin(
        "inline fun identity(value: Int): Int = value\n\
         fun broken(): Int = missing\n",
    )
    .with_file_stem("PassBoundary")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("explicit signatures and the inline body must finalize in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert_eq!(census.failures.len(), 1);
    assert_eq!(
        census.failures[0].stage,
        crate::compiler::FrontendStage::Check
    );
    assert_eq!(diagnostics.diags.len(), 1);
    assert_eq!(diagnostics.diags[0].file, 0);
    assert_eq!(diagnostics.diags[0].span, crate::diag::Span::new(65, 72));
    assert_eq!(diagnostics.diags[0].msg, "unresolved reference 'missing'.");
}

#[test]
fn inferred_signature_reports_a_missing_member_during_pass_one() {
    let inputs = [SourceInput::kotlin(
        "class Token(val text: String)\n\
         class Container {\n\
             val Token.marker get() = text\n\
             fun read(token: Token) = token.marker.missing\n\
         }\n",
    )
    .with_file_stem("MissingSignatureMember")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    let streamed = analysis
        .streamed
        .as_ref()
        .expect("an invalid signature must still publish compact diagnostic Pass-2 state");
    assert!(streamed.diagnostic_recovery);
    assert!(!streamed.module.index().retains_source_coordinates());
    assert!(
        (0..streamed.module.index().declaration_count()).any(|raw| {
            let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
            streamed
                .module
                .index()
                .declaration_header(declaration)
                .is_some()
                && streamed.module.index().signature(declaration).is_none()
        }),
        "the failed signature must be absent rather than represented by Pending/Error"
    );
    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    assert_eq!(diagnostics.diags[0].file, 0);
    assert_eq!(diagnostics.diags[0].msg, "unresolved reference 'missing'.");
}

#[test]
fn inferred_member_extension_result_keeps_dispatch_type_arguments() {
    let inputs = [SourceInput::kotlin(
        "class Wrapper<T>(val value: T)\n\
         open class Container<T>(private val value: T) {\n\
             val String.wrapped get() = Wrapper(value)\n\
         }\n\
         class StringContainer(value: String) : Container<String>(value) {\n\
             fun read(token: String): Int = token.wrapped.value.length\n\
         }\n",
    )
    .with_file_stem("AppliedMemberExtension")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some());
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn emission_signature_collection_does_not_reject_valid_init_order_member_calls() {
    let inputs = [SourceInput::kotlin(
        "class Initialized {
             var observed: Int = 0
             init { initialize() }
             val later: Int = 1
             fun initialize() { observed = 1 }
         }
         fun build(): Initialized = Initialized()
",
    )
    .with_file_stem("InitOrder")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some());
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_resolves_local_type_parameter_annotations_in_lexical_class_scope() {
    let inputs = [SourceInput::kotlin(
        "class Host {
             annotation class Mark
             fun run(): String {
                 fun <@Mark T> keep(value: T): T = value
                 return keep(\"OK\")
             }
         }
",
    )
    .with_file_stem("LocalAnnotation")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_uses_stable_header_flags_for_top_level_val_smart_casts() {
    let inputs = [SourceInput::kotlin(
        "val minus: Any = -0.0\n\
         fun box(): String {\n\
             if (minus is Comparable<*> && minus is Double) {\n\
                 if (minus < 0.0) return \"fail\"\n\
             }\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("StableTopLevelVal")];
    let mut diagnostics = DiagSink::new();
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            crate::toolchain::classpath_jars_for("// WITH_REFLECT"),
        )),
    ));
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.files.is_empty(), "Pass 1 AST must be released");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inferred_object_extension_call_shapes_an_unused_implicit_it_in_both_passes() {
    let inputs = [SourceInput::kotlin(
        "inline fun <T> T.keep(block: (T) -> Unit): T = this\n\
         object Token\n\
         fun inferred() = Token.keep { val ignored = 0 }\n",
    )
    .with_file_stem("ObjectExtension")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "Pass 1 signatures must finalize"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn emission_pass_one_does_not_check_ordinary_members_beside_an_inline_member() {
    let inputs = [SourceInput::kotlin(
        "class Owner {\n\
             inline fun identity(value: Int): Int = value\n\
             fun broken(): Int = missing\n\
         }\n",
    )
    .with_file_stem("InlineMember")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the class header and inline member must finalize in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert_eq!(census.failures.len(), 1);
    assert_eq!(
        census.failures[0].stage,
        crate::compiler::FrontendStage::Check
    );
    assert_eq!(diagnostics.diags.len(), 1);
    assert_eq!(diagnostics.diags[0].file, 0);
    assert_eq!(diagnostics.diags[0].span, crate::diag::Span::new(79, 86));
    assert_eq!(diagnostics.diags[0].msg, "unresolved reference 'missing'.");
}

#[test]
fn unresolved_body_local_classifier_header_is_checked_in_pass_two() {
    let source = "fun box(): String {\n\
                      class Local(val value: Missing)\n\
                      return \"OK\"\n\
                  }\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("UnresolvedLocalHeader")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    let start = source.find("Missing").expect("missing type spelling") as u32;
    assert!(
        diagnostics.diags.is_empty(),
        "an ordinary local header must not be checked during Pass 1: {:?}",
        diagnostics.diags
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert_eq!(census.failures.len(), 1, "{:?}", census.failures);
    assert_eq!(
        census.failures[0].stage,
        crate::compiler::FrontendStage::Check
    );
    assert_eq!(census.failures[0].source, 0);
    assert_eq!(
        census.failures[0].span,
        Some(crate::diag::Span::new(start, start + 7))
    );
    assert_eq!(census.failures[0].kind, "Rejected");
    assert_eq!(census.failures[0].detail, "unresolved reference 'Missing'.");
    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    assert_eq!(diagnostics.diags[0].file, 0);
    assert_eq!(
        diagnostics.diags[0].span,
        crate::diag::Span::new(start, start + 7)
    );
    assert_eq!(diagnostics.diags[0].msg, "unresolved reference 'Missing'.");
}

#[test]
fn emission_pass_one_defers_ordinary_anonymous_captures_to_the_active_file() {
    let inputs = [SourceInput::kotlin(
        "interface Label { fun text(): String }\n\
         fun build(): Label {\n\
             val value = \"ready\"\n\
             return object : Label { override fun text(): String = value }\n\
         }\n",
    )
    .with_file_stem("DeferredCapture")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.reparse_sources[0].released_before_collection(),
        "ordinary anonymous/local classifier bodies must be represented by compact lexical context before module signature collection"
    );
    let index = analysis
        .streamed
        .as_ref()
        .expect("non-local signatures must finalize without checking the ordinary body")
        .module
        .index();
    let anonymous = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.kind == crate::fir::DeclarationKind::Classifier
                        && header
                            .flags
                            .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
                })
        })
        .expect("the anonymous classifier must have a stable body-local identity");
    let children = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| anchor.owner == Some(anonymous))
        })
        .collect::<Vec<_>>();
    assert!(!children.is_empty());
    assert!(children.iter().all(|declaration| {
        index
            .declaration_header(*declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS))
    }));
    assert!(children.iter().all(|declaration| {
        index.declaration_header(*declaration).is_none_or(|header| {
            !header
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
        })
    }));

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_publishes_anonymous_context_method_parameters_without_double_offset() {
    let inputs = [SourceInput::kotlin(
        "interface LoggingContext\n\
         interface Repository<T> {\n\
             context(context: LoggingContext)\n\
             fun save(content: T)\n\
         }\n\
         class Value\n\
         fun box(): String {\n\
             val repository = object : Repository<Value> {\n\
                 context(context: LoggingContext)\n\
                 override fun save(content: Value) {}\n\
             }\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("AnonymousContextMethod")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_checks_inline_owned_anonymous_members_from_compact_capture_context() {
    let inputs = [SourceInput::kotlin(
        "interface Mapper<T : Any> { fun map(value: T): String }\n\
         class Factory {\n\
             companion object {\n\
                 inline fun <reified T : Any> build(\n\
                     crossinline transform: (T) -> String,\n\
                 ): Mapper<T> = object : Mapper<T> {\n\
                     override fun map(value: T): String {\n\
                         return transform(value)\n\
                     }\n\
                 }\n\
             }\n\
         }\n",
    )
    .with_file_stem("InlineOwnedAnonymous")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inline owner must be retained in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_keeps_inline_anonymous_members_enclosing_extension_receiver_label() {
    let inputs = [SourceInput::kotlin(
        "interface Flow<T> { suspend fun collect(collector: FlowCollector<T>) }\n\
         fun interface FlowCollector<T> { suspend fun emit(value: T) }\n\
         inline fun <T, R> Flow<T>.transform(\n\
             crossinline block: suspend FlowCollector<R>.(T) -> Unit\n\
         ): Flow<R> = object : Flow<R> {\n\
             override suspend fun collect(collector: FlowCollector<R>) {\n\
                 this@transform.collect { value -> collector.block(value) }\n\
             }\n\
         }\n",
    )
    .with_file_stem("InlineAnonymousExtensionReceiver")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inline owner must be retained in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_invokes_captured_extension_function_value_with_explicit_receiver_argument() {
    let inputs = [SourceInput::kotlin(
        "interface Sink<E>\n\
         interface Source<E> { fun consume(sink: Sink<E>) }\n\
         inline fun <E> source(\n\
             crossinline action: Sink<E>.() -> Unit,\n\
         ): Source<E> = object : Source<E> {\n\
             override fun consume(sink: Sink<E>) {\n\
                 action(sink)\n\
             }\n\
         }\n",
    )
    .with_file_stem("InlineCapturedExtensionInvoke")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inline owner must be retained in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_invokes_captured_extension_function_value_with_qualified_receiver() {
    let inputs = [SourceInput::kotlin(
        "interface Sink<E>\n\
         interface Source<E> { fun consume(sink: Sink<E>) }\n\
         inline fun <E> source(\n\
             crossinline action: Sink<E>.() -> Unit,\n\
         ): Source<E> = object : Source<E> {\n\
             override fun consume(sink: Sink<E>) {\n\
                 sink.action()\n\
             }\n\
         }\n",
    )
    .with_file_stem("InlineCapturedQualifiedExtensionInvoke")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inline owner must be retained in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_finalizes_ordinary_anonymous_member_signatures_without_retaining_bodies() {
    let inputs = [SourceInput::kotlin(
        "interface WriteContext\n\
         interface ReadContext\n\
         interface Codec<T> {\n\
             fun WriteContext.encode(value: T)\n\
             fun ReadContext.decode(): T?\n\
         }\n\
         fun <T> codec(\n\
             encode: WriteContext.(T) -> Unit,\n\
             decode: ReadContext.() -> T?,\n\
         ): Codec<T> = object : Codec<T> {\n\
             override fun WriteContext.encode(value: T) = encode(value)\n\
             override fun ReadContext.decode(): T? = decode()\n\
         }\n",
    )
    .with_file_stem("LocalInvokeExtension")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "explicit public signatures must finalize without checking body-local overrides"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn undemanded_anonymous_member_signature_remains_pass_two_lexical_work() {
    let inputs = [SourceInput::kotlin(
        "fun box(): String {\n\
             val captured = \"a\"\n\
             val value = object {\n\
                 override fun toString(): String = foo(captured) + foo(\"b\")\n\
                 fun foo(text: String) = text + text\n\
             }\n\
             return value.toString()\n\
         }\n",
    )
    .with_file_stem("AnonymousSiblingSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("anonymous member signatures must finalize in Pass 1")
        .module
        .index();
    let foo = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("foo"))
        .expect("anonymous foo declaration");
    assert!(
        index.signature(foo).is_none(),
        "an ordinary local member not demanded by a non-local signature must not be retained from Pass 1"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_records_local_anonymous_hierarchy_for_pass_two_super_selection() {
    let inputs = [SourceInput::kotlin(
        "fun box(): String {\n\
             open class Outer {\n\
                 open inner class A {\n\
                     open fun foo(x: String, y: String? = null): String = x + (y ?: \"K\")\n\
                 }\n\
             }\n\
             val value = object : Outer() {\n\
                 inner class MyClass : A() {\n\
                     override fun foo(x: String, y: String?) = super.foo(x, y)\n\
                 }\n\
             }\n\
             return value.MyClass().foo(\"O\")\n\
         }\n",
    )
    .with_file_stem("LocalAnonymousHierarchy")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "compact local classifier headers must finalize before ordinary body checking"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inferred_package_qualified_inline_call_shapes_its_lambda_before_body_streaming() {
    let inputs = [
        SourceInput::kotlin(
            "package lib\n\
             inline fun apply(block: () -> String): String = block()\n",
        )
        .with_file_stem("Library"),
        SourceInput::kotlin("fun box() = lib.apply { \"OK\" }\n").with_file_stem("Main"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the package-qualified inferred signature must finalize");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inferred_nested_classifier_companion_property_keeps_the_classifier_qualifier() {
    let inputs = [SourceInput::kotlin(
        "class Outer {\n\
             class Nested {\n\
                 companion object { val answer = \"OK\" }\n\
             }\n\
         }\n\
         fun box() = Outer.Nested.answer\n",
    )
    .with_file_stem("NestedCompanion")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the nested companion property must finalize the inferred result"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inferred_qualified_enum_entry_property_keeps_the_enum_value_prefix() {
    let inputs = [SourceInput::kotlin(
        "enum class Choice {\n\
             OK;\n\
             val text = \"OK\"\n\
         }\n\
         fun box() = Choice.OK.text\n",
    )
    .with_file_stem("EnumEntryValue")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the enum-entry-qualified inferred signature must finalize"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn enum_entry_initializer_call_keeps_stable_member_and_receiver_decisions() {
    let inputs = [SourceInput::kotlin(
        "enum class Choice {\n\
             OK {\n\
                 fun ping() {}\n\
                 init { ping() }\n\
             };\n\
         }\n",
    )
    .with_file_stem("EnumEntryCall")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn enum_entry_generic_member_signature_finalizes_from_compact_headers() {
    let inputs = [SourceInput::kotlin(
        "enum class Choice {\n\
             OK {\n\
                 fun <T : CharSequence> keep(value: T): T = value\n\
                 val token = keep(\"OK\")\n\
             };\n\
         }\n",
    )
    .with_file_stem("EnumEntryGenericMember")];
    let mut diagnostics = DiagSink::new();
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
        )),
    ));
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the header-only enum-entry member must finalize in Pass 1"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn enum_entry_generic_member_signature_honors_source_subtype_bound() {
    let inputs = [SourceInput::kotlin(
        "interface Marker\n\
         class Token : Marker\n\
         enum class Choice {\n\
             OK {\n\
                 fun <T : Marker> keep(value: T): T = value\n\
                 val token = keep(Token())\n\
             };\n\
         }\n",
    )
    .with_file_stem("EnumEntryGenericSourceBound")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the source-bounded enum-entry member must finalize in Pass 1"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn object_name_call_uses_the_selected_singleton_invoke_extension() {
    let inputs = [SourceInput::kotlin(
        "object Token\n\
         operator fun Token.invoke(value: Int): Int = value\n\
         fun answer() = Token(42)\n",
    )
    .with_file_stem("ObjectInvoke")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inferred_generic_constructor_uses_the_completed_lambda_result_type() {
    let inputs = [SourceInput::kotlin(
        "class Element(val value: Int)\n\
         class Holder<T>(val factory: (Int) -> T)\n\
         val holder = Holder { Element(42) }\n\
         fun read(): Int = holder.factory(0).value\n",
    )
    .with_file_stem("GenericConstructorLambda")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn concrete_constructor_argument_solves_before_symbolic_outer_expectation() {
    let inputs = [SourceInput::kotlin(
        "class Cell<T : Any>(val value: T?)\n\
         fun <T : Any> same(left: Cell<T>, right: Cell<T>?): Boolean = left == right\n\
         fun check(): Boolean = same(Cell(0), Cell(1))\n",
    )
    .with_file_stem("NestedConstructorInference")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_reparse_removes_the_complete_actualized_expect_class_subtree() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect class A {\n\
                 constructor()\n\
                 inner class B {\n\
                     fun answer(): Int\n\
                     constructor()\n\
                 }\n\
             }\n",
        )
        .with_file_stem("Common"),
        SourceInput::kotlin(
            "actual class A {\n\
                 actual inner class B actual constructor() {\n\
                     actual fun answer(): Int = 42\n\
                 }\n\
             }\n\
             fun box(): Int = A().B().answer()\n",
        )
        .with_file_stem("Actual"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "Pass 1 must finalize actual signatures"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn source_optional_expectation_keeps_finalized_constructor_and_file_suppression() {
    let source = "// WITH_STDLIB\n\
                  // LANGUAGE: +MultiPlatformProjects\n\
                  @file:Suppress(\"OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE\")\n\
                  import kotlin.OptionalExpectation as MayDisappear\n\
                  @MayDisappear\n\
                  expect annotation class Optional()\n\
                  @Optional fun answer(): String = \"OK\"\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Optional")];
    let mut classpath = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &LangFeatures::from_source(source),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let classifier = index
        .classifier_declaration(crate::types::type_name("Optional"))
        .expect("target-less optional expectation must remain a common semantic declaration");
    let constructor = index
        .owned_declaration(classifier, crate::fir::DeclarationKind::Constructor, 0)
        .expect("optional annotation constructor declaration");
    assert!(
        index.signature(constructor).is_some(),
        "Pass 2 must consume the finalized annotation constructor signature"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn actualization_keeps_a_distinct_common_overload_after_body_compaction() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect fun f(param: Int): String\n\
             fun f(param: Any): String = \"any\"\n",
        )
        .with_file_stem("Common")
        .common(),
        SourceInput::kotlin(
            "actual fun f(param: Int): String = \"int\"\n\
             fun box(): String = f(1) + f(\"s\")\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn actualization_selects_overloads_through_an_actual_typealias() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect class S\n\
             expect fun f(value: S): S\n\
             expect fun f(value: Int): Int\n\
             expect val S.tag: S\n\
             expect val Int.tag: Int\n\
             fun common(value: S): S = f(value).tag\n",
        )
        .with_file_stem("Common")
        .common(),
        SourceInput::kotlin(
            "actual fun f(value: String): String = value\n\
             actual fun f(value: Int): Int = value\n\
             actual val String.tag: String get() = this\n\
             actual val Int.tag: Int get() = this\n\
             actual typealias S = String\n\
             fun box(): String = common(\"OK\")\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inferred_common_result_keeps_exact_actual_overload_identity() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect class S\n\
             expect fun f(value: S): S\n\
             expect fun f(value: Int): Int\n\
             fun common(value: S) = f(value)\n",
        )
        .with_file_stem("Common")
        .common(),
        SourceInput::kotlin(
            "actual fun f(value: Int) = value\n\
             actual fun f(value: String) = value\n\
             actual typealias S = String\n\
             fun box(): String = common(\"OK\")\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn intermediate_actual_keeps_the_expect_type_until_a_later_typealias() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect fun f(value: S): S\n\
             expect class S\n",
        )
        .with_file_stem("Common0")
        .common(),
        SourceInput::kotlin("actual fun f(value: S): S = value\n")
            .with_file_stem("Common1")
            .common(),
        SourceInput::kotlin(
            "actual typealias S = String\n\
             fun box(): String = f(\"OK\")\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn anonymous_unit_cannot_rebind_an_earlier_top_level_classifier_header() {
    let inputs = [SourceInput::kotlin(
        "class A(val value: String)\n\
         fun box(): String {\n\
             val nested = object { val inner = object { val a = A(\"OK\") } }\n\
             return nested.inner.a.value\n\
         }\n",
    )
    .with_file_stem("AnonymousUnit")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn actualized_class_keeps_expect_constructor_and_member_defaults_in_pass_two() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect class C(value: String, count: Int = 0) {\n\
                 fun render(prefix: String = \"value\"): String\n\
             }\n",
        )
        .with_file_stem("Common"),
        SourceInput::kotlin(
            "actual class C actual constructor(value: String, count: Int) {\n\
                 actual fun render(prefix: String): String = prefix\n\
             }\n\
             fun box(): String = C(\"OK\").render()\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_default_resolves_an_imported_sibling_source_package() {
    let inputs = [
        SourceInput::kotlin(
            "import helpers.*\n\
             fun selected(value: Int = provide()): Int = value\n",
        )
        .with_file_stem("Main"),
        SourceInput::kotlin(
            "package helpers\n\
             fun provide(): Int = 42\n",
        )
        .with_file_stem("Helpers"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_body_resolves_a_platform_class_from_the_finalized_module_index() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect interface I { fun use(value: Int = 1) }",
        )
        .common()
        .with_file_stem("Common"),
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             actual interface I { actual fun use(value: Int) }\n\
             interface C { fun use(value: Int) {} }\n\
             class G(c: C) : C by c, I\n\
             fun box(): G = G(object : C {})",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let classifier = crate::types::type_name("G");
    index
        .classifier_declaration(crate::types::type_name("G"))
        .expect("the platform classifier must have a stable module identity");
    assert!(
        index
            .constructor_declaration(classifier, true, &[crate::types::Ty::obj("C")])
            .is_some(),
        "the finalized classifier must expose its primary constructor"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_anonymous_object_retains_its_own_property_surface() {
    use crate::symbol_source::SymbolSource;

    let inputs = [SourceInput::kotlin(
        "fun box(): String {\n\
             val value = object { lateinit var x: Any }\n\
             return if (value.x == null) \"FAIL\" else \"OK\"\n\
         }",
    )
    .with_file_stem("AnonymousProperty")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let internal = crate::types::type_name("AnonymousPropertyKt$box$1");
    let legacy = analysis
        .symbols
        .class_by_type_name(internal)
        .expect("the temporary local-class projection must exist");
    assert!(legacy.declared_props.contains_key("x"));
    let stable_classifiers = (0..index.declaration_count())
        .filter_map(|raw| {
            index
                .classifier_header(crate::fir::DeclarationId::from_raw(raw as u32))
                .map(|classifier| classifier.classifier)
        })
        .collect::<Vec<_>>();
    assert!(
        index.classifier_declaration(internal).is_some(),
        "anonymous classifier identity mismatch: expected={}, stable={:?}",
        internal.render(),
        stable_classifiers
            .iter()
            .map(|classifier| classifier.render())
            .collect::<Vec<_>>()
    );
    let (namespace, segment) = crate::symbol_source::SymbolNamespace::classifier_key(internal);
    assert_eq!(
        namespace.existing_classifier(segment),
        Some(internal),
        "classifier namespace decomposition must preserve the generated identity: namespace={namespace:?} segment={segment}"
    );
    let provider = crate::fir::StreamedModuleSymbols::for_file(index, 0);
    let classifier = provider
        .classifier(internal)
        .expect("Pass 2 must expose the stable anonymous classifier");
    assert!(
        classifier
            .declared_callables
            .get("x")
            .is_some_and(|callables| !callables.properties().is_empty()),
        "the stable classifier provider must expose the anonymous object's property"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn non_local_property_signature_publishes_demanded_anonymous_member_surface() {
    use crate::symbol_source::SymbolSource;

    let inputs = [SourceInput::kotlin(
        "abstract class A {\n\
             private val x = object { fun foo() = \"OK\" }\n\
             protected val y = x.foo()\n\
         }\n\
         class B : A() { val z = y }\n\
         fun box() = B().z",
    )
    .with_file_stem("AnonymousMemberProperty")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let anonymous = crate::types::type_name("A$1");
    let provider = crate::fir::StreamedModuleSymbols::for_file(index, 0);
    let classifier = provider
        .classifier(anonymous)
        .expect("the anonymous classifier must have a stable header");
    assert!(
        classifier
            .declared_callables
            .get("foo")
            .is_some_and(|callables| !callables.functions().is_empty()),
        "a demanded inferred member must survive as stable callable metadata"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn streamed_classifier_members_do_not_depend_on_legacy_callable_maps() {
    use crate::symbol_source::SymbolSource;

    let inputs = [SourceInput::kotlin(
        "interface Marker\n\
         class Token : Marker\n\
         class Box<T>(var value: T) {\n\
             fun <R : Marker> keep(candidate: R): R = candidate\n\
         }\n\
         fun box(): Marker = Box(Token()).keep(Token())",
    )
    .with_file_stem("StableMembers")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let internal = crate::types::type_name("Box");

    let provider = crate::fir::StreamedModuleSymbols::for_file(index, 0);
    let classifier = provider
        .classifier(internal)
        .expect("stable classifier must survive removal of legacy callable maps");
    let keep = classifier
        .declared_callables
        .get("keep")
        .expect("stable generic member function must be projected")
        .functions();
    assert_eq!(keep.len(), 1);
    assert!(keep[0].stable_declaration.is_some());
    assert_eq!(
        keep[0]
            .generic_sig
            .as_ref()
            .expect("generic member signature")
            .formals
            .len(),
        1
    );
    let value = classifier
        .declared_callables
        .get("value")
        .expect("stable member property must be projected")
        .properties();
    assert_eq!(value.len(), 1);
    assert!(value[0].stable_declaration.is_some());
    assert!(value[0].setter.is_some());
    assert!(value[0].ty.mentions_ty_param());
    drop(finish_pass_one(analysis));
}

#[test]
fn streamed_cross_file_companion_const_keeps_checked_payload_on_selected_property() {
    use crate::symbol_source::{SymbolNamespace, SymbolSource};

    let inputs = [
        SourceInput::kotlin(
            "package lib\n\
             class Limits { companion object { const val MAX: Int = 42 } }",
        )
        .with_file_stem("Limits"),
        SourceInput::kotlin(
            "package use\n\
             import lib.Limits\n\
             fun read(): Int = Limits.MAX",
        )
        .with_file_stem("Use"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let provider = crate::fir::StreamedModuleSymbols::for_file(index, 1);
    let companion = provider
        .classifier(crate::types::type_name("lib/Limits$Companion"))
        .expect("stable companion classifier");
    let property = companion
        .declared_callables
        .get("MAX")
        .expect("stable companion property")
        .properties()
        .first()
        .expect("one stable companion property");
    assert!(property.compile_time_constant.is_some());

    let associated = provider.symbols(
        SymbolNamespace::Classifier(crate::types::type_name("lib/Limits")),
        "MAX",
    );
    assert!(
        associated
            .callables
            .properties()
            .iter()
            .any(|property| property.compile_time_constant.is_some()),
        "classifier-qualified lookup must retain the selected const payload"
    );
}

#[test]
fn same_file_actual_keeps_expect_default_after_expect_removal_changes_sibling_ordinal() {
    let inputs = [SourceInput::kotlin(
        "// LANGUAGE: +MultiPlatformProjects\n\
         expect fun withLimit(limit: Long = 42L): Long\n\
         actual fun withLimit(limit: Long): Long = limit\n\
         fun box(): Long = withLimit()\n",
    )
    .with_file_stem("Actual")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn same_file_suspend_actual_keeps_expect_default_in_production_stream() {
    let source = "// LANGUAGE: +MultiPlatformProjects\n\
                  expect suspend fun withLimit(limit: Long = 42L)\n\
                  actual suspend fun withLimit(limit: Long) {}\n\
                  suspend fun use() { withLimit() }\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("DefaultExpect")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_streaming_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "signatures must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn actualized_declarations_keep_callable_reference_and_lambda_expect_defaults() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect fun top(block: () -> String = { \"OK\" }): String\n\
             expect class Foo {\n\
                 val p: Int\n\
                 fun bar(read: () -> Int = this::p): Int\n\
             }\n",
        )
        .with_file_stem("Common"),
        SourceInput::kotlin(
            "actual fun top(block: () -> String): String = block()\n\
             actual class Foo {\n\
                 actual val p: Int = 42\n\
                 actual fun bar(read: () -> Int): Int = read()\n\
             }\n\
             fun box(): String = if (top() == \"OK\" && Foo().bar() == 42) \"OK\" else \"FAIL\"\n",
        )
        .with_file_stem("Actual"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn actualized_function_keeps_defaults_that_reference_prior_parameters() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             class B(val value: Int)\n\
             expect fun test(\n\
                 a: Int = 2,\n\
                 b: Int = B(a * 2).value,\n\
                 c: String = \"${b}$a\",\n\
             ): String\n",
        )
        .with_file_stem("Common")
        .common(),
        SourceInput::kotlin(
            "actual fun test(a: Int, b: Int, c: String): String = c\n\
             fun box(): String = test()\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn nested_actualized_generic_member_inherits_expect_default_by_stable_ownership() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect class A<T> {\n\
                 inner class B<N> {\n\
                     fun <H> foo(\n\
                         t: T, n: N, h: H,\n\
                         a: (T, N, H) -> Int = { _, _, _ -> 4 },\n\
                     ): Int\n\
                 }\n\
             }\n",
        )
        .with_file_stem("Common")
        .common(),
        SourceInput::kotlin(
            "actual class A<T> {\n\
                 actual inner class B<N> {\n\
                     actual fun <H> foo(t: T, n: N, h: H, a: (T, N, H) -> Int) = a(t, n, h)\n\
                 }\n\
             }\n\
             fun box(): Int = A<Int>().B<Double>().foo<Int>(1, 2.0, 3)\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();
    let provider = crate::fir::StreamedModuleSymbols::for_file(index, 1);
    let inner =
        crate::symbol_source::SymbolSource::classifier(&provider, crate::types::type_name("A$B"))
            .expect("stable actual inner classifier");
    assert_eq!(
        inner.outer_instance,
        Some(crate::types::type_name("A")),
        "inner receiver must be a stable classifier fact"
    );
    assert!(
        !inner.constructors.is_empty(),
        "inner constructors must be projected without ClassSig"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn actual_typealias_to_object_is_a_singleton_value_in_pass_two() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             expect class TestResult\n\
             expect fun make(): TestResult\n",
        )
        .with_file_stem("Common"),
        SourceInput::kotlin(
            "actual typealias TestResult = Unit\n\
             actual fun make(): TestResult = TestResult\n\
             fun box(): String = if (make() == Unit) \"OK\" else \"FAIL\"\n",
        )
        .with_file_stem("Actual"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(inputs[0].text),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_typealias_resolution_uses_the_finalized_index() {
    let inputs = [SourceInput::kotlin(
        "class Cell<T>(val value: T)\n\
         class Pair<A, B>(val first: A, val second: B)\n\
         typealias TextCell = Cell<String>\n\
         typealias StringPair<B> = Pair<String, B>\n\
         fun stable(value: TextCell): TextCell = value\n\
         fun box(): String = TextCell(\"O\").value + StringPair<String>(\"K\", \"\").first\n",
    )
    .with_file_stem("StableAlias")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let index = analysis.streamed.as_ref().unwrap().module.index();
    let stable = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("stable"))
        .expect("the declared function must have a stable identity");
    let spelling = index
        .declaration_spellings(stable)
        .expect("stable declaration spellings must be published before legacy state is released");
    assert_eq!(
        spelling.ret.alias,
        Some(crate::types::type_name("TextCell"))
    );
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_resolves_and_validates_an_explicit_enum_entry_import() {
    let inputs = [SourceInput::kotlin(
        "package sample\n\
         import sample.Choice.ONE\n\
         enum class Choice { ONE, TWO }\n\
         fun chosen(): Choice = ONE\n",
    )
    .with_file_stem("ImportedEnumEntry")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn dependency_receiver_function_typealias_keeps_its_function_shape() {
    let inputs = [
        SourceInput::kotlin(
            "private data class NumberWithString<N : Number>(val n: N, val s: String)\n\
             private fun <N : Number> use(\n\
                 ns: NumberWithString<N>,\n\
                 process: ProcessOverriddenWithBaseScope<N>\n\
             ): String {\n\
                 val (n, s) = ns\n\
                 val result = s.process(n) { _, value -> value == \"OK\" }\n\
                 return if (result) \"OK\" else \"FAIL\"\n\
             }\n\
             fun box(): String = use(NumberWithString(42, \"OK\")) { n, process ->\n\
                 process(n, \"OK\")\n\
             }\n",
        )
        .with_file_stem("ReceiverAliasUse"),
        SourceInput::kotlin(
            "typealias ProcessOverriddenWithBaseScope<D> =\n\
                 String.(D, (D, String) -> Boolean) -> Boolean\n",
        )
        .with_file_stem("ReceiverAliasDependency"),
    ];
    let mut diagnostics = DiagSink::new();
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
        )),
    ));
    let analysis = analyze_source_set_with_features_and_prepare_prefix(
        &inputs,
        1,
        1,
        platform,
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn qualified_alias_of_body_local_class_remains_pass_two_lexical_state() {
    let source = "// LANGUAGE: +NestedTypeAliases +LocalTypeAliases\n\
                  fun box(): String {\n\
                      class Local {\n\
                          val value: String get() = \"OK\"\n\
                          typealias Alias = Local\n\
                          fun make(): String = Alias().value\n\
                      }\n\
                      val value: Local.Alias = Local.Alias()\n\
                      return value.make()\n\
                  }\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("BodyLocalNestedAlias")];
    let features = LangFeatures::from_source(source);
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &features,
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn compact_constructor_completion_preserves_integer_literal_bound_adaptation() {
    let source = "// LANGUAGE: +GenericInlineClassParameter\n\
                  value class Bound<T : Long>(val value: T)\n\
                  val inferred = Bound(0)\n\
                  fun accept(value: Bound<Long>): Long = value.value\n\
                  fun box(): String {\n\
                      accept(inferred)\n\
                      accept(Bound(0))\n\
                      return \"OK\"\n\
                  }\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("ConstructorLiteralBound")];
    let features = LangFeatures::from_source(source);
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &features,
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn retained_inline_member_annotations_are_checked_under_the_enclosing_suppression() {
    let inputs = [
        SourceInput::kotlin("private annotation class Hidden").with_file_stem("HiddenAnnotation"),
        SourceInput::kotlin(
            "@Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\")\n\
             class Container {\n\
                 @Hidden private inline fun value(): String = \"OK\"\n\
                 fun read(): String = value()\n\
             }\n\
             fun box(): String = Container().read()\n",
        )
        .with_file_stem("RetainedInlineAnnotation"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn contextual_smartcast_signature_keeps_the_declaration_type_parameter() {
    let inputs = [SourceInput::kotlin(
        "// LANGUAGE: +ContextSensitiveResolutionUsingExpectedType\n\
         sealed interface Either<out E, out A> {\n\
             data class Left<out E>(val error: E) : Either<E, Nothing>\n\
             data class Right<out A>(val value: A) : Either<Nothing, A>\n\
         }\n\
         fun <E, A> Either<E, A>.getOrElse(default: A) = when (this) {\n\
             is Left -> default\n\
             is Right -> value\n\
         }\n\
         fun box(): String = Either.Right(\"OK\").getOrElse(\"fail\")\n",
    )
    .with_file_stem("ContextualSmartcast")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the generic signature must finalize"
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn statement_suppression_applies_while_streaming_only_its_body_subtree() {
    let inputs = [SourceInput::kotlin(
        "class Hidden { private val status: String = \"OK\" }\n\
         fun status(value: Hidden): String {\n\
             @Suppress(\"INVISIBLE_MEMBER\")\n\
             if (value is Hidden) return value.status\n\
             return \"NO STATUS\"\n\
         }\n",
    )
    .with_file_stem("ScopedSuppression")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn file_suppression_seeds_each_streamed_body_lexical_scope() {
    let inputs = [SourceInput::kotlin(
        "@file:Suppress(\"INVISIBLE_MEMBER\")\n\
         class Hidden { private val status: String = \"OK\" }\n\
         fun status(value: Hidden): String = value.status\n",
    )
    .with_file_stem("FileSuppression")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn data_object_does_not_publish_a_generated_copy_candidate() {
    let inputs = [SourceInput::kotlin(
        "data object A { fun copy() = \"O\" }\n\
         data object B { fun copy(test: String) = test }\n\
         fun box(): String = A.copy() + B.copy(\"K\")\n",
    )
    .with_file_stem("DataObjectCopy")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn delegated_superclass_member_is_a_concrete_super_selection() {
    let inputs = [SourceInput::kotlin(
        "interface Foo { fun bar(x: Int, y: String? = null): String }\n\
         open class FooFoo(val delegate: Foo) : Foo by delegate\n\
         class Final(delegate: Foo) : FooFoo(delegate) {\n\
             override fun bar(x: Int, y: String?): String = super.bar(x, y)\n\
         }\n",
    )
    .with_file_stem("DelegatedSuper")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn emission_pass_one_discovers_captures_only_inside_retained_inline_bodies() {
    let inputs = [SourceInput::kotlin(
        "interface Label { fun text(): String }\n\
         inline fun inlineBuild(): Label {\n\
             val value = \"inline\"\n\
             return object : Label { override fun text(): String = value }\n\
         }\n\
         fun ordinaryBuild(): Label {\n\
             val value = \"ordinary\"\n\
             return object : Label { override fun text(): String = value }\n\
         }\n",
    )
    .with_file_stem("InlineCaptureBoundary")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inline body and non-local signatures must finalize in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    let generated_capture_fields = (0..streamed.module.index().declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            streamed
                .module
                .index()
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.kind == crate::fir::DeclarationKind::Property
                        && header
                            .flags
                            .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
                        && header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                })
        })
        .count();
    assert_eq!(generated_capture_fields, 1);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_inline_fir_publishes_owned_anonymous_object_members() {
    let inputs = [SourceInput::kotlin(
        r#"inline fun build(crossinline value: () -> String) =
               object { fun read() = value() }.read()
           class Use {
               val result = build(::answer)
               fun answer() = "OK"
           }"#,
    )
    .with_file_stem("InlineAnonymousMember")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("inline FIR with an anonymous child must finalize in Pass 1");
    let index = streamed.module.index();
    assert_eq!(
        streamed.module.inline_bodies().len(),
        1,
        "callables={:?}",
        (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .filter_map(|declaration| {
                index.callable_for_declaration(declaration).map(|callable| {
                    (
                        declaration,
                        callable.clone(),
                        index
                            .signature(declaration)
                            .map(|signature| signature.result.get()),
                    )
                })
            })
            .collect::<Vec<_>>()
    );
    assert!(
        (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .all(|declaration| index.declaration_header(declaration).is_some()),
        "finalized Pass-1 declarations must not retain headerless duplicate identities",
    );
    let anonymous = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header
                        .flags
                        .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
                })
        })
        .expect("anonymous classifier must retain a stable identity");
    let inline_owner = index
        .declaration_anchor(anonymous)
        .and_then(|anchor| anchor.owner)
        .expect("anonymous classifier must be owned by its inline function");
    assert!(index
        .callable_for_declaration(inline_owner)
        .is_some_and(crate::fir::ResolvedCallableHeader::is_inline));
    let local_methods = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| anchor.owner == Some(anonymous))
        })
        .filter(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| header.kind == crate::fir::DeclarationKind::Function)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        local_methods.len(),
        1,
        "anonymous={anonymous:?}, declarations={:?}",
        (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .map(|declaration| (
                declaration,
                index.declaration_anchor(declaration),
                index
                    .declaration_header(declaration)
                    .map(|header| header.kind),
                index.callable_for_declaration(declaration).is_some(),
            ))
            .collect::<Vec<_>>()
    );
    assert!(index.callable_for_declaration(local_methods[0]).is_some());
    assert_eq!(
        index.signature(local_methods[0]).unwrap().result.get(),
        Ty::String,
        "the inferred anonymous-member result must be published under its stable identity before the retained inline FIR is checked",
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_inline_anonymous_member_retains_its_local_classifier_subtree() {
    let inputs = [SourceInput::kotlin(
        r#"inline fun build(): String = object {
               fun read(): String {
                   abstract class A
                   fun local() {}
                   open class B
                   class C
                   data class D(val value: Int)
                   local()
                   B()
                   C()
                   D(1)
                   return "OK"
               }
           }.read()"#,
    )
    .with_file_stem("InlineAnonymousLocalClassifiers")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_inline_fir_reads_an_inferred_anonymous_property_by_its_canonical_identity() {
    let inputs = [SourceInput::kotlin(
        r#"inline fun <reified T> result() = object { val value = "OK" }.value
           fun use(): String = result<String>()"#,
    )
    .with_file_stem("InlineAnonymousProperty")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inferred anonymous property must be checked with its inline owner");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    let index = streamed.module.index();
    let headerless = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_header(*declaration).is_none())
        .map(|declaration| (declaration, index.declaration_anchor(declaration)))
        .collect::<Vec<_>>();
    assert!(
        headerless.is_empty(),
        "anonymous-property scanning must not intern headerless ancestry aliases: {headerless:?}",
    );
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .expect("stable result declaration");
    assert_eq!(index.signature(result).unwrap().result.get(), Ty::String);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_retains_an_inline_member_nested_in_an_ordinary_property_initializer() {
    let inputs = [SourceInput::kotlin(
        r#"object Holder {
               private val callable = object {
                   inline operator fun invoke(): String = "OK"
               }
               fun value() = callable()
           }
           fun box(): String = Holder.value()"#,
    )
    .with_file_stem("InlineInOrdinaryInitializer")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the nested inline member must bind to its stable anonymous owner");
    assert_eq!(streamed.module.inline_bodies().len(), 1);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_inline_check_publishes_an_inferred_generic_anonymous_override() {
    let inputs = [SourceInput::kotlin(
        r#"interface Transform<A : Any, B : Any> { fun apply(value: A): B }
           object Factory {
               inline fun <reified A : Any, reified B : Any> build(
                   crossinline transform: (A) -> B,
               ): Transform<A, B> = object : Transform<A, B> {
                   override fun apply(value: A) = transform(value)
               }
           }"#,
    )
    .with_file_stem("InlineGenericAnonymousOverride")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inferred anonymous override must finalize with its inline owner");
    let index = streamed.module.index();
    let apply = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index.declaration_name(*declaration) == Some("apply")
                && index
                    .declaration_header(*declaration)
                    .is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    })
        })
        .expect("anonymous apply declaration");
    assert!(index.signature(apply).is_some());

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn emission_pass_one_retains_inline_accessor_fir_only() {
    let inputs = [SourceInput::kotlin(
        "class Accessors {\n\
             val computed: Int inline get() = 1\n\
             var stored: Int = 0\n\
                 inline get() = field\n\
                 inline set(value) { field = value }\n\
             fun ordinary(): Int = computed + stored\n\
         }\n",
    )
    .with_file_stem("InlineAccessors")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("inline accessor signatures and bodies must finalize in Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 3);
    let inline_accessors = (0..streamed.module.index().declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            streamed
                .module
                .index()
                .declaration_header(*declaration)
                .is_some_and(|header| header.kind == crate::fir::DeclarationKind::Accessor)
        })
        .filter(|declaration| {
            streamed
                .module
                .index()
                .callable_for_declaration(*declaration)
                .is_some_and(crate::fir::ResolvedCallableHeader::is_inline)
        })
        .count();
    assert_eq!(inline_accessors, 3);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inline_accessor_binding_ignores_released_sibling_body_spans() {
    let inputs = [SourceInput::kotlin(
        "private var state = 0\n\
         private var value: Int\n\
             inline get() = state\n\
             set(next) { state = next }\n\
         fun box(): String { value = 1; return if (value == 1) \"OK\" else \"fail\" }\n",
    )
    .with_file_stem("MixedInlineAccessors")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn signature_default_binding_ignores_released_ordinary_local_class_ownership() {
    let inputs = [SourceInput::kotlin(
        "fun select(value: Int = 1): Int {\n\
             val ordinary = object {\n\
                 fun nested(): Any = object {}\n\
             }\n\
             return value\n\
         }\n\
         class Holder(val value: Int = 2) {\n\
             val next = value + 1\n\
             init { next }\n\
         }\n\
         fun box(): String = if (select() + Holder().value == 3) \"OK\" else \"fail\"\n",
    )
    .with_file_stem("CompactedDefaultOwners")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_streaming_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn production_pass_one_publishes_stable_pending_free_signatures() {
    let source = "class Box(val value: Int) {\n\
                          constructor(text: String) : this(text.length)\n\
                          fun label(): String = \"box\"\n\
                          val count: Long = 1L\n\
                      }\n\
                      fun use(value: Int): String = value.toString()\n\
                      fun inferred() = \"ready\"\n\
                      inline fun inlined(value: Int): Int = value + 1\n\
                      val answer: Long = 42L\n\
                      val inferredAnswer = 7\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Stable")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("valid production analysis must cross the stable Pass-1 boundary");
    assert_eq!(streamed.module.sources().len(), 1);
    assert_eq!(streamed.module.index().len(), 10);
    assert!(streamed.module.index().declaration_count() >= streamed.module.index().len());
    assert_eq!(streamed.module.inline_bodies().len(), 1);
}

#[test]
fn production_pass_one_publishes_checked_const_expression_payloads_only_for_const_declarations() {
    let source = "const val later = 2\n\
                  const val answer = later + 2\n\
                  val ordinary = 2 + 2\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Constants")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let properties = analysis.files[0]
        .decls
        .iter()
        .filter_map(|declaration| match analysis.files[0].decl(*declaration) {
            crate::ast::Decl::Property(property) => Some((property.name.as_str(), *declaration)),
            crate::ast::Decl::Fun(_) | crate::ast::Decl::Class(_) => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let constant = |name: &str| {
        analysis.symbols.source_props[&(0, properties[name].0)]
            .compile_time_constant
            .clone()
    };
    assert_eq!(
        constant("answer"),
        Some(crate::libraries::LibraryConst {
            ty: crate::types::Ty::Int,
            value: crate::libraries::LibConst::Int(4),
        })
    );
    assert_eq!(
        constant("later"),
        Some(crate::libraries::LibraryConst {
            ty: crate::types::Ty::Int,
            value: crate::libraries::LibConst::Int(2),
        })
    );
    assert_eq!(constant("ordinary"), None);
}

#[test]
fn production_signature_graph_solves_a_deferred_call_before_its_member_lookup() {
    let source = "fun a() = b().length\nfun b() = \"hello\"\nfun c() = number() + 1\nfun number() = 41\nfun neg() = -number()\nfun cmp() = number() < 42\nfun logic() = true && false\nfun id(value: String) = value\nfun useId() = id(\"ok\")\nfun <T> genericId(value: T) = value\nfun useGeneric() = genericId(\"generic\")\nfun choose(flag: Boolean) = if (flag) number() else 0L\nfun local(flag: Boolean) = if (flag) { val value = b(); value } else \"fallback\"\nfun safe(value: String?) = value?.length\nfun String.extLength() = length\nclass Holder(val text: String) { fun measured() = text.length; fun forwarded() = measured() }\nclass Box<T>(val value: T) { fun get() = value; fun <R> echo(value: R) = value }\nfun <T> Box<T>.extensionGet() = value\nfun readBox(box: Box<String>) = box.get()\nfun memberGeneric(box: Box<String>) = box.echo(7)\nfun readExtension(box: Box<String>) = box.extensionGet()\nvar counter = 0\nfun next() = counter++\nfun first(values: Array<String>) = values[0]\nval factory = { \"made\" }\nfun make() = factory()\nfun applyFactory(factory: () -> String) = factory()\nfun cast(value: Any) = value as String\nfun typedLocal(flag: Boolean) = if (flag) { val value: String = \"typed\"; value } else \"fallback\"\nfun <T> typedGeneric(flag: Boolean, value: T) = if (flag) { val copy: T = value; copy } else value\nval typedLambda = { value: String -> value }\nfun invokeTyped() = typedLambda(\"typed\")\ndata class Pairish(val text: String, val count: Int)\nfun destructured(flag: Boolean, value: Pairish) = if (flag) { val (text, _) = value; text } else \"fallback\"";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Lazy")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the lazy graph must finalize before body checking");
    let declaration = |name: &str| {
        (0..streamed.module.index().declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .filter(|declaration| {
                streamed.module.index().declaration_name(*declaration) == Some(name)
                    && streamed.module.index().signature(*declaration).is_some()
            })
            .max_by_key(|declaration| {
                streamed
                    .module
                    .index()
                    .declaration_header(*declaration)
                    .is_some()
            })
            .expect("stable named function declaration")
    };
    let result = |name| {
        streamed
            .module
            .index()
            .signature(declaration(name))
            .unwrap()
            .result
            .get()
    };
    for (name, expected) in [
        ("b", Ty::String),
        ("cast", Ty::String),
        ("typedLocal", Ty::String),
        ("invokeTyped", Ty::String),
        ("destructured", Ty::String),
        ("applyFactory", Ty::String),
        ("make", Ty::String),
        ("next", Ty::Int),
        ("first", Ty::String),
        ("readBox", Ty::String),
        ("memberGeneric", Ty::Int),
        ("readExtension", Ty::String),
        ("safe", Ty::nullable(Ty::Int)),
        ("extLength", Ty::Int),
        ("forwarded", Ty::Int),
        ("measured", Ty::Int),
        ("useGeneric", Ty::String),
        // `number()` is a fixed Int expression and `0L` is a fixed Long literal. Kotlinc's
        // inferred source result is Any (and rejects assigning `choose(flag)` to Long).
        ("choose", Ty::obj("kotlin/Any")),
        ("local", Ty::String),
        ("useId", Ty::String),
        ("a", Ty::Int),
        ("neg", Ty::Int),
        ("cmp", Ty::Boolean),
        ("logic", Ty::Boolean),
        ("id", Ty::String),
        ("c", Ty::Int),
    ] {
        assert_eq!(result(name), expected, "{name}");
    }
    for name in ["get", "typedGeneric"] {
        assert!(matches!(result(name), Ty::TyParam(_, _)), "{name}");
    }
}

#[test]
fn production_signature_graph_resolves_delegated_property_conventions() {
    let source = "class Delegate<T>(val value: T) {\n\
                          operator fun getValue(thisRef: Any?, property: Any): T = value\n\
                      }\n\
                      val delegated by Delegate(\"delegated\")\n\
                      fun readDelegated() = delegated\n\
                      fun readLocalDelegated() = if (true) {\n\
                          val local by Delegate(\"local\")\n\
                          local\n\
                      } else \"fallback\"";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Delegate")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("delegated convention inference must finalize before Pass 2");
    let read = analysis.files[0]
        .decls
        .iter()
        .find_map(|declaration| match analysis.files[0].decl(*declaration) {
            crate::ast::Decl::Fun(function) if function.name == "readDelegated" => {
                Some(function.span)
            }
            crate::ast::Decl::Fun(_)
            | crate::ast::Decl::Class(_)
            | crate::ast::Decl::Property(_) => None,
        })
        .unwrap();
    let declaration =
        stable_declaration_at(&analysis, 0, read, crate::fir::DeclarationKind::Function);
    assert_eq!(
        streamed
            .module
            .index()
            .signature(declaration)
            .unwrap()
            .result
            .get(),
        Ty::String,
    );
    let local_read = analysis.files[0]
        .decls
        .iter()
        .find_map(|declaration| match analysis.files[0].decl(*declaration) {
            crate::ast::Decl::Fun(function) if function.name == "readLocalDelegated" => {
                Some(function.span)
            }
            crate::ast::Decl::Fun(_)
            | crate::ast::Decl::Class(_)
            | crate::ast::Decl::Property(_) => None,
        })
        .unwrap();
    let local_declaration = stable_declaration_at(
        &analysis,
        0,
        local_read,
        crate::fir::DeclarationKind::Function,
    );
    assert_eq!(
        streamed
            .module
            .index()
            .signature(local_declaration)
            .unwrap()
            .result
            .get(),
        Ty::String,
    );
}

#[test]
fn production_signature_graph_preserves_explicit_anonymous_function_shapes() {
    let source = "fun receiverFactory() = fun String.(): Int = this.length\n\
                      fun suspendFactory() = suspend { \"ready\" }\n\
                      fun declaredFactory() = fun(value: String): String = value\n\
                      fun localFactory() = if (true) {\n\
                          fun build(value: String) = value\n\
                          build(\"local\")\n\
                      } else \"fallback\"\n\
                      fun smart(value: Any) = if (value is String) value.length else 0\n\
                      fun negatedSmart(value: Any) = if (value !is String) 0 else value.length\n\
                      fun nonNull(value: String?) = if (value != null) value.length else 0\n\
                      fun nullElse(value: String?) = if (value == null) 0 else value.length\n\
                      fun whenSmart(value: Any) = when (value) { is String -> value.length; else -> 0 }";
    let inputs = [SourceInput::kotlin(source).with_file_stem("FunctionShapes")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("explicit anonymous-function shapes must finalize before Pass 2");
    let signature = |name: &str| {
        let span = analysis.files[0]
            .decls
            .iter()
            .find_map(|declaration| match analysis.files[0].decl(*declaration) {
                crate::ast::Decl::Fun(function) if function.name == name => Some(function.span),
                crate::ast::Decl::Fun(_)
                | crate::ast::Decl::Class(_)
                | crate::ast::Decl::Property(_) => None,
            })
            .unwrap();
        streamed
            .module
            .index()
            .signature(stable_declaration_at(
                &analysis,
                0,
                span,
                crate::fir::DeclarationKind::Function,
            ))
            .unwrap()
            .result
            .get()
    };

    let Ty::Fun(receiver) = signature("receiverFactory") else {
        panic!("receiver factory must infer a function type")
    };
    assert!(receiver.has_receiver);
    assert_eq!(receiver.params.as_slice(), &[Ty::String]);
    assert_eq!(receiver.ret, Ty::Int);
    let Ty::Fun(suspend) = signature("suspendFactory") else {
        panic!("suspend factory must infer a function type")
    };
    assert!(suspend.suspend);
    assert_eq!(suspend.params.as_slice(), &[]);
    assert_eq!(suspend.ret, Ty::String);
    let Ty::Fun(declared) = signature("declaredFactory") else {
        panic!("declared factory must infer a function type")
    };
    assert_eq!(declared.params.as_slice(), &[Ty::String]);
    assert_eq!(declared.ret, Ty::String);
    assert_eq!(signature("localFactory"), Ty::String);
    for name in ["smart", "negatedSmart", "nonNull", "nullElse", "whenSmart"] {
        assert_eq!(signature(name), Ty::Int, "{name}");
    }
}

#[test]
fn production_signature_graph_demands_an_inferred_callable_reference_target() {
    let source = r#"
            fun referenced() = "reference"
            val functionReference = ::referenced
            fun invokeReference() = functionReference()
            class ReferenceHolder { fun text() = "member" }
            fun boundReference(holder: ReferenceHolder) = holder::text
            fun invokeBound(holder: ReferenceHolder) = boundReference(holder)()
        "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("CallableReference")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("an unambiguous callable reference must finalize before Pass 2");
    for name in ["invokeReference", "invokeBound"] {
        let invoke = analysis.files[0]
            .decls
            .iter()
            .find_map(|declaration| match analysis.files[0].decl(*declaration) {
                crate::ast::Decl::Fun(function) if function.name == name => Some(function.span),
                crate::ast::Decl::Fun(_)
                | crate::ast::Decl::Class(_)
                | crate::ast::Decl::Property(_) => None,
            })
            .unwrap();
        let declaration =
            stable_declaration_at(&analysis, 0, invoke, crate::fir::DeclarationKind::Function);
        assert_eq!(
            streamed
                .module
                .index()
                .signature(declaration)
                .unwrap()
                .result
                .get(),
            Ty::String,
        );
    }
}

#[test]
fn signature_graph_uses_only_the_current_files_private_outer_callable() {
    let inputs = [
        SourceInput::kotlin(
            "class V { fun target(x: String = \"x\", y: String = \"y\"): String = x + y }\n\
             private fun capture(value: (String, String) -> String): Any = value\n",
        )
        .with_file_stem("First"),
        SourceInput::kotlin(
            "private fun capture(value: (String, String) -> String): Any = value\n\
             fun inferred(value: V) = capture(value::target)\n",
        )
        .with_file_stem("Second"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the inferred cross-file call must finalize before Pass 2");
    let inferred = analysis.files[1]
        .decls
        .iter()
        .find_map(|declaration| match analysis.files[1].decl(*declaration) {
            crate::ast::Decl::Fun(function) if function.name == "inferred" => Some(function.span),
            crate::ast::Decl::Fun(_)
            | crate::ast::Decl::Class(_)
            | crate::ast::Decl::Property(_) => None,
        })
        .expect("inferred function");
    let declaration = stable_declaration_at(
        &analysis,
        1,
        inferred,
        crate::fir::DeclarationKind::Function,
    );
    assert_eq!(
        streamed
            .module
            .index()
            .signature(declaration)
            .expect("resolved signature")
            .result
            .get(),
        Ty::obj("kotlin/Any"),
    );
}

#[test]
fn production_signature_graph_uses_compact_alias_and_star_import_scopes() {
    let inputs = [
        SourceInput::kotlin(
            "package values\n\
             fun text() = \"ready\"\n",
        )
        .with_file_stem("Values"),
        SourceInput::kotlin(
            "package aliasuse\n\
             import values.text as selectedText\n\
             fun aliasLength() = selectedText().length\n",
        )
        .with_file_stem("AliasUse"),
        SourceInput::kotlin(
            "package staruse\n\
             import values.*\n\
             fun starLength() = text().length\n",
        )
        .with_file_stem("StarUse"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("compact import scopes must solve before Pass 2")
        .module
        .index();
    for (source, name) in [(1usize, "aliasLength"), (2, "starLength")] {
        let span = analysis.files[source]
            .decls
            .iter()
            .find_map(
                |declaration| match analysis.files[source].decl(*declaration) {
                    crate::ast::Decl::Fun(function) if function.name == name => Some(function.span),
                    crate::ast::Decl::Fun(_)
                    | crate::ast::Decl::Class(_)
                    | crate::ast::Decl::Property(_) => None,
                },
            )
            .expect("consumer function");
        let declaration = stable_declaration_at(
            &analysis,
            source,
            span,
            crate::fir::DeclarationKind::Function,
        );
        assert_eq!(index.signature(declaration).unwrap().result.get(), Ty::Int);
    }
}

#[test]
fn inferred_source_constructor_projects_argument_supertypes_before_finalization() {
    let source = "interface Source<out T>\n\
                  class Values : Source<String>\n\
                  class Box<T>(value: Source<T>)\n\
                  val box = Box(Values())\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("ProjectedConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("projected constructor inference must finalize before Pass 2")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| {
                    anchor.kind == crate::fir::DeclarationKind::Property
                        && index.declaration_name(*declaration) == Some("box")
                })
        })
        .expect("inferred box property");
    assert_eq!(
        index.signature(declaration).unwrap().result.get(),
        Ty::obj_args("Box", &[Ty::String]),
    );
}

#[test]
fn signature_graph_rejects_an_eager_same_file_forward_read_before_pass_two() {
    let source = "val eager = later\nval later = 1";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Forward")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let streamed = analysis
        .streamed
        .as_ref()
        .expect("a failed forward-read signature must retain compact diagnostic state");
    assert!(streamed.diagnostic_recovery);
    assert!(!streamed.module.index().retains_source_coordinates());
    assert!(
        (0..streamed.module.index().declaration_count()).any(|raw| {
            let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
            streamed.module.index().declaration_name(declaration) == Some("eager")
                && streamed.module.index().signature(declaration).is_none()
        }),
        "the rejected eager property must not publish an error signature"
    );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| (diagnostic.file, diagnostic.span, diagnostic.msg.as_str()))
            .collect::<Vec<_>>(),
        [(
            0,
            Span::new(12, 17),
            "variable 'later' must be initialized."
        )],
    );
}

#[test]
fn nested_anonymous_inner_constructor_default_is_checked_and_owned_in_pass_one() {
    let inputs = [SourceInput::kotlin(
        "fun box(): String {\n\
             val owner = object {\n\
                 inner class Value(val text: String = \"OK\")\n\
             }\n\
             return owner.Value().text\n\
         }\n",
    )
    .with_file_stem("NestedDefault")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the local constructor default must finish Pass 1");
    assert_eq!(streamed.module.default_arguments().len(), 1);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn local_constructor_default_captures_enclosing_class_header_parameter() {
    let inputs = [SourceInput::kotlin(
        "open class Base(val fn: () -> String)\n\
         class Test(x: String) :\n\
             Base({\n\
                 class Local(val text: String = x)\n\
                 Local().text\n\
             })\n\
         fun box(): String = Test(\"OK\").fn()\n",
    )
    .with_file_stem("CapturedHeaderDefault")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the captured local constructor default must finish Pass 1");
    assert_eq!(streamed.module.default_arguments().len(), 1);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn constructor_default_publishes_local_classifier_members_checked_inside_the_default() {
    let inputs = [SourceInput::kotlin(
        "fun <T> eval(fn: () -> T): T = fn()\n\
         class A(val text: String = eval {\n\
             open class B { open fun value(): String = \"O\" }\n\
             val derived = object : B() { override fun value(): String = \"K\" }\n\
             B().value() + derived.value()\n\
         })\n\
         fun box(): String = A().text\n",
    )
    .with_file_stem("DefaultLocalClassifier")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the default's checked local signatures must finish Pass 1");
    assert_eq!(streamed.module.default_arguments().len(), 1);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn secondary_constructor_default_is_selected_from_the_stable_parameter_header() {
    let inputs = [SourceInput::kotlin(
        "open class Base(val value: Any) {\n\
             constructor(text: String = \"OK\") : this(text)\n\
         }\n\
         object Derived : Base()\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("StableSecondaryDefault")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("the secondary constructor default must finish Pass 1");
    assert_eq!(streamed.module.default_arguments().len(), 1);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_top_level_property_stability_uses_the_active_stable_declaration() {
    let inputs = [SourceInput::kotlin(
        "class Item { fun value(): String = \"OK\" }\n\
         val padding: Int = 0\n\
         var current: Item? = null\n\
         fun box(): String {\n\
             current?.value()\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("StableTopLevelPropertyPath")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn nested_anonymous_member_default_is_fully_published_from_the_retained_default() {
    let inputs = [SourceInput::kotlin(
        "interface Value { fun text(): String }\n\
         fun read(value: Value = object : Value {\n\
             fun nested(inner: Value = object : Value {\n\
                 override fun text() = \"OK\"\n\
             }): String = inner.text()\n\
             override fun text(): String = nested()\n\
         }): String = value.text()\n\
         fun box(): String = read()\n",
    )
    .with_file_stem("NestedAnonymousDefault")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("both nested defaults must finish Pass 1");
    assert_eq!(streamed.module.default_arguments().len(), 2);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn default_check_does_not_reopen_released_declaration_annotation_arguments() {
    let inputs = [SourceInput::kotlin(
        "annotation class Mark(val text: String)\n\
         @Mark(\"header-only\") class Value(val text: String = \"OK\")\n\
         fun box(): String = Value().text\n",
    )
    .with_file_stem("AnnotatedDefault")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_local_member_result_is_joined_by_stable_declaration() {
    let inputs = [SourceInput::kotlin(
        "open class Base\n\
         fun run(text: String): String {\n\
             class Local : Base {\n\
                 constructor(value: Int) : super() {}\n\
                 constructor(value: String) : super() {}\n\
                 fun result() = text\n\
             }\n\
             return Local(1).result() + Local(\"ignored\").result()\n\
         }\n\
         fun box(): String = run(\"OK\")\n",
    )
    .with_file_stem("StableLocalMemberResult")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_local_constructor_result_keeps_captured_type_parameter() {
    let inputs = [SourceInput::kotlin(
        "interface Identity<T> { fun read(value: T): T = value }\n\
         fun <T> outer(value: T): T {\n\
             fun local(): T {\n\
                 class Holder : Identity<T>\n\
                 fun sibling(): T = Holder().read(value)\n\
                 return sibling()\n\
             }\n\
             return local()\n\
         }\n\
         fun box(): String = outer(\"OK\")\n",
    )
    .with_file_stem("StableLocalCapturedConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_inner_constructor_uses_finalized_captured_parameter_shape() {
    let inputs = [SourceInput::kotlin(
        "interface Sequence<T>\n\
         class Concrete<T>(val value: T) : Sequence<T>\n\
         class Container<T>(private val values: Sequence<T>) {\n\
             inner class Cursor(private val sequence: Sequence<T>)\n\
             fun cursor(): Cursor = Cursor(values)\n\
         }\n\
         val source: Sequence<String> = Concrete(\"OK\")\n\
         val strings = Container(source)\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("StableInnerConstructorShape")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_secondary_constructor_respects_explicit_classifier_arguments() {
    let inputs = [SourceInput::kotlin(
        "class Choice<T>(val value: T) {\n\
             constructor(number: Double) : this(\"OK\" as T)\n\
         }\n\
         fun box(): String = Choice<String>(1.0).value\n",
    )
    .with_file_stem("StableExplicitSecondaryConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_constructor_combines_contravariant_argument_and_expected_supertype_constraints() {
    let inputs = [SourceInput::kotlin(
        "interface Consumer<in T> { fun accept(value: T) }\n\
         class Wrapped<T>(private val original: Consumer<T>) : Consumer<T> {\n\
             override fun accept(value: T) { original.accept(value) }\n\
         }\n\
         fun <T> wrap(original: Consumer<T>): Consumer<T> = Wrapped(original)\n\
         interface Named\n\
         object ConcreteName : Named\n\
         class Box<T : Named>(val value: T)\n\
         fun boxed(): Box<Named> = Box(ConcreteName)\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("ContravariantConstructorResult")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_expected_supertype_contextualizes_a_nested_constructor_lambda() {
    let inputs = [SourceInput::kotlin(
        "interface Cursor<T> { fun hasNext(): Boolean; fun next(): T }\n\
         interface Values<T> { fun iterator(): Cursor<T> }\n\
         class Yield<T>(val factory: () -> (() -> T?)) : Values<T> {\n\
             override fun iterator(): Cursor<T> = null as Cursor<T>\n\
         }\n\
         fun <TItem> Values<TItem>.lazy(): Values<TItem> = Yield {\n\
             val iterator = this.iterator();\n\
             { if (iterator.hasNext()) iterator.next() else null }\n\
         }\n",
    )
    .with_file_stem("ExpectedSupertypeNestedLambda")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_constructor_bound_contextualizes_a_postponed_builder_result() {
    let inputs = [SourceInput::kotlin(
        "class Target\n\
         class Buildee<T> { fun set(value: T) {} }\n\
         fun <T> build(instructions: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()\n\
         class Holder<T : Any>(val buildee: Buildee<T>)\n\
         fun consume(value: Buildee<Any>) {}\n\
         fun box(): String {\n\
             val holder = Holder(build { set(Target()) })\n\
             consume(holder.buildee)\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("PostponedBuilderConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_anonymous_function_spells_a_postponed_extension_receiver_parameter() {
    let inputs = [SourceInput::kotlin(
        "class Target\n\
         class Buildee<T> { fun yield(value: T) {} }\n\
         fun <T> build(instructions: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()\n\
         fun consume(value: Buildee<Target>) {}\n\
         fun box(): String {\n\
             val buildee = build(fun(it) { it.yield(Target()) })\n\
             consume(buildee)\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("AnonymousFunctionBuilder")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_constructor_retains_a_lambda_body_inference_solution() {
    let inputs = [SourceInput::kotlin(
        "class Holder<T : Any>(val block: (Holder<T>.() -> Unit)? = null) {\n\
             var consumer: ((T) -> Unit)? = null\n\
         }\n\
         fun consumeInt(value: Int) {}\n\
         fun box(): String {\n\
             Holder { consumer = { consumeInt(it) } }\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("ConstructorLambdaInference")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_constructor_merges_fixed_and_inferred_type_arguments() {
    let inputs = [SourceInput::kotlin(
        "class Box<E : Double, A : Any>(val value: A)\n\
         fun consume(value: Box<Double, String>) {}\n\
         fun box(): String {\n\
             consume(Box<Double, _>(\"answer\"))\n\
             Box<Nothing, _>(2.0)\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("PartialExplicitConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_constructor_infers_from_an_explicitly_typed_lambda_parameter() {
    let inputs = [SourceInput::kotlin(
        "class Filter<T>(val predicate: (T) -> Boolean) {\n\
             fun accepts(value: T): Boolean = predicate(value)\n\
         }\n\
         fun box(): String {\n\
             if (!Filter({ value: Int -> value < 5 }).accepts(2)) return \"fail\"\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("TypedLambdaConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_typealias_constructor_infers_omitted_arguments_from_selected_context() {
    let inputs = [SourceInput::kotlin(
        "class Owner<T : Any> {\n\
             fun <X : Any> constrain(source: PairType<X, T>): PairType<X, T> = source\n\
         }\n\
         fun <T : Any> pcla(block: Owner<T>.() -> PairType<*, T>) {}\n\
         class PairType<A : Any, B : Any>\n\
         class Concrete\n\
         typealias Source<Y> = PairType<Y, Concrete>\n\
         fun box(): String {\n\
             pcla { constrain(Source()) }\n\
             return \"OK\"\n\
         }\n",
    )
    .with_file_stem("ContextualTypealiasConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_constructor_infers_integer_literal_from_the_class_parameter_bound() {
    let inputs = [SourceInput::kotlin(
        "class LongBox<T : Long>(val value: T)\n\
         class IntBox<T : Int>(val value: T)\n\
         fun inferred(): LongBox<Long> {\n\
             val local = LongBox(1)\n\
             return local\n\
         }\n\
         fun <T : Int> lexicallyExpected(): IntBox<T> = IntBox(-1)\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("BoundedIntegerLiteralConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_secondary_constructor_infers_its_class_type_argument() {
    let inputs = [SourceInput::kotlin(
        "class Box<T>(val first: T, val second: T) {\n\
             constructor(value: T) : this(value, value)\n\
         }\n\
         fun make(): Any {\n\
             val box = Box(\"answer\")\n\
             return box\n\
         }\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("GenericSecondaryConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_constructor_preserves_a_lexically_fixed_class_type_argument() {
    let inputs = [SourceInput::kotlin(
        "class Wrapper<T : Int>(val value: T) {\n\
             fun replaced(value: T): Wrapper<T> = Wrapper(value)\n\
         }\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("LexicalGenericConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_no_arg_constructor_uses_a_symbolic_local_expected_type() {
    let inputs = [SourceInput::kotlin(
        "class Wrapper<T> {\n\
             fun make() {\n\
                 fun local() {\n\
                     val value: Wrapper<T> = Wrapper()\n\
                 }\n\
                 local()\n\
             }\n\
         }\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("SymbolicExpectedConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_interface_name_does_not_preempt_a_same_named_factory() {
    let inputs = [SourceInput::kotlin(
        "interface Channel<T>\n\
         fun <T> Channel(): Channel<T> = object : Channel<T> {}\n\
         class Holder<T>(val channel: Channel<T> = Channel())\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("InterfaceFactory")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_object_typealias_selects_the_singleton_invoke() {
    let inputs = [SourceInput::kotlin(
        "object Callable { operator fun invoke(): String = \"OK\" }\n\
         typealias Alias = Callable\n\
         fun box(): String = Alias()\n",
    )
    .with_file_stem("ObjectAliasInvoke")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_super_constructor_contextualizes_a_nested_generic_constructor() {
    let inputs = [SourceInput::kotlin(
        "class Value<out T>(val holder: String)\n\
         open class Container<E>(val value: Value<E>)\n\
         fun make(): Any = object : Container<String>(Value(\"OK\")) {}\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("ContextualSuperConstructorArgument")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_equality_contextualizes_an_otherwise_unbound_generic_constructor() {
    let inputs = [SourceInput::kotlin(
        "class Optional<T : Any>(val value: T?)\n\
         fun same(known: Optional<String>): Boolean = known == Optional(null)\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("ContextualEqualityConstructor")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_call_parameter_contextualizes_a_bare_enum_entry() {
    let inputs = [SourceInput::kotlin(
        "// LANGUAGE: +ContextSensitiveResolutionUsingExpectedType\n\
         enum class Mode { FIRST, SECOND }\n\
         fun select(mode: Mode): Mode = mode\n\
         fun chosen(): Mode = select(SECOND)\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("ContextualEnumArgument")];
    let mut diagnostics = DiagSink::new();
    let features = LangFeatures::from_source(inputs[0].text);
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &features,
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_call_parameter_contextualizes_an_empty_reference_array_factory() {
    let inputs = [SourceInput::kotlin(
        "fun consume(values: Array<Int>): Int = values.size\n\
         fun emptySize(): Int = consume(arrayOf())\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("ContextualEmptyArrayFactory")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_classifier_identity_shadows_same_spelled_array_synthetic() {
    let inputs = [SourceInput::kotlin(
        "class UIntArray(val storage: IntArray)\n\
         fun wrap(storage: IntArray): UIntArray = UIntArray(storage)\n\
         fun box(): String = \"OK\"\n",
    )
    .with_file_stem("ArraySyntheticShadow")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_inferred_function_result_updates_only_its_stable_declaration() {
    let inputs = [
        SourceInput::kotlin(
            "class First<T>(val first: T)\n\
             class Last<T>(val last: T)\n",
        )
        .with_file_stem("Results"),
        SourceInput::kotlin(
            "fun first() = First(\"OK\")\n\
             fun last() = Last(1)\n\
             fun box(): String = first().first\n",
        )
        .with_file_stem("Calls"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn nested_inline_check_does_not_reopen_its_released_enclosing_body() {
    let inputs = [SourceInput::kotlin(
        "fun add(left: Int, right: Int): Int {\n\
             val operation = object {\n\
                 inline fun add(left: Int, right: Int): Int = left + right\n\
             }\n\
             return operation.add(left, right)\n\
         }\n\
         fun box(): String = if (add(1, 2) == 3) \"OK\" else \"fail\"\n",
    )
    .with_file_stem("NestedInline")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn nested_classifier_inside_local_class_publishes_inferred_member_result() {
    let inputs = [SourceInput::kotlin(
        "fun box(): String {\n\
             val capture = \"oh\"\n\
             class Local {\n\
                 val captured = capture\n\
                 open inner class Inner(\n\
                     val d: Double = -1.0,\n\
                     val s: String,\n\
                     vararg val y: Int,\n\
                 ) {\n\
                     open fun result() = \"Fail\"\n\
                 }\n\
                 val obj = object : Inner(s = \"OK\") {\n\
                     override fun result() = s\n\
                 }\n\
             }\n\
             return Local().obj.result()\n\
         }\n",
    )
    .with_file_stem("NestedClassifierInLocalClass")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn bounded_reparse_binds_multiple_local_classifier_subtrees_in_source_order() {
    let inputs = [SourceInput::kotlin(
        "fun padding() {\n\
             val one = 1\n\
             val two = 2\n\
             if (one != two) { val three = 3; three + one }\n\
         }\n\
         fun exercise(): String {\n\
             fun first() {\n\
                 class Local\n\
                 Local()\n\
             }\n\
             fun second(): String {\n\
                 class Local { fun result() = \"OK\" }\n\
                 return Local().result()\n\
             }\n\
             first()\n\
             return second()\n\
         }\n\
         fun box(): String = exercise()\n",
    )
    .with_file_stem("MultipleLocalClassifierSubtrees")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn excluded_expect_annotation_policy_never_reopens_released_arguments() {
    let inputs = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             @Target(AnnotationTarget.CLASS)\n\
             expect annotation class Mark constructor()\n\
             @Mark class CommonValue\n",
        )
        .with_file_stem("Common")
        .common(),
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
             @Target(AnnotationTarget.CLASS)\n\
             actual annotation class Mark\n\
             fun box(): String = \"OK\"\n",
        )
        .with_file_stem("Actual"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
}

#[test]
fn stable_member_result_never_falls_back_to_active_parser_coordinates() {
    let inputs = [SourceInput::kotlin(
        "interface I { fun call(): Int }\n\
         class A(val value: Any?) {\n\
             fun empty(): Boolean = value == null\n\
             fun call(): Int = (value as? I)?.call() ?: 0\n\
         }\n\
         fun box(): String = if (A(null).call() == 0) \"OK\" else \"fail\"\n",
    )
    .with_file_stem("StableMemberResult")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_callable_references_use_stable_declarations_without_emit_owners() {
    let inputs = [SourceInput::kotlin(
        "fun direct(value: String): String = value\n\
         fun defaulted(value: String = \"OK\"): String = value\n\
         fun String.extension(): String = this\n\
         fun box(): String {\n\
             val directReference: (String) -> String = ::direct\n\
             val adaptedReference: () -> String = ::defaulted\n\
             val extensionReference: (String) -> String = String::extension\n\
             return directReference(extensionReference(adaptedReference()))\n\
         }\n",
    )
    .with_file_stem("StableCallableReferences")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn reparsed_contextual_collection_literal_replaces_its_fallback_call_target() {
    let inputs = [SourceInput::kotlin(
        "// WITH_STDLIB\n\
         // LANGUAGE: +CollectionLiterals\n\
         class MyList(val values: Array<out String>) {\n\
             companion object {\n\
                 operator fun of(vararg values: String): MyList = MyList(values)\n\
             }\n\
         }\n\
         fun consume(values: MyList): String = values.values.joinToString(\"\")\n\
         fun box(): String = consume([\"O\", \"K\"])\n",
    )
    .with_file_stem("ContextualCollectionLiteral")];
    let features = LangFeatures::from_source(inputs[0].text);
    let mut classpath = crate::toolchain::classpath_jars_for(inputs[0].text);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &features,
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let analysis = finish_pass_one(analysis);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn constructor_parameter_coercion_annotation_survives_bounded_reparse() {
    let inputs = [
        SourceInput::kotlin(
            "package kotlin.internal\n\
             annotation class ImplicitIntegerCoercion\n",
        )
        .with_file_stem("ImplicitIntegerCoercion"),
        SourceInput::kotlin(
            "// LANGUAGE: +ImplicitSignedToUnsignedIntegerConversion\n\
             import kotlin.internal.ImplicitIntegerCoercion\n\
             class Unsigned {\n\
                 constructor(@ImplicitIntegerCoercion value: UInt)\n\
                 constructor(value: String)\n\
             }\n\
             fun box(): String { Unsigned(42); return \"OK\" }\n",
        )
        .with_file_stem("UnsignedCall"),
    ];
    let features = LangFeatures::from_source(inputs[1].text);
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &features,
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let coercing = (0..index.declaration_count()).find_map(|raw| {
        let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
        let header = index.declaration_header(declaration)?;
        if header.kind != crate::fir::DeclarationKind::Constructor
            || index.signature(declaration)?.parameters.first()?.get() != crate::types::Ty::UInt
        {
            return None;
        }
        let callable = index.callable_for_declaration(declaration)?;
        index.callable_parameter(callable.id, 0)
    });
    assert!(
        coercing
            .expect("UInt constructor parameter")
            .flags()
            .has_implicit_integer_coercion(),
        "resolved compiler-known parameter behavior must survive without constructor syntax"
    );
}

#[test]
fn bounded_body_binding_allows_distinct_declarations_with_shared_active_span() {
    let inputs = [SourceInput::kotlin(
        "interface Context\n\
         interface Continuation<T> {\n\
             val context: Context\n\
             fun resume(value: T)\n\
         }\n\
         fun box(): String {\n\
             var result = \"fail\"\n\
             val continuation = object : Continuation<String> {\n\
                 override fun resume(value: String) { result = value }\n\
                 override val context: Context\n\
                     get() = object : Context {}\n\
             }\n\
             continuation.resume(\"OK\")\n\
             return result\n\
         }\n",
    )
    .with_file_stem("SharedActiveBodySpan")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn retained_inline_body_publishes_constructors_of_lexical_local_classes() {
    let inputs = [SourceInput::kotlin(
        "inline fun value(): String = object {\n\
             fun read(): String {\n\
                 open class Base\n\
                 class Derived : Base()\n\
                 data class Payload(val value: String)\n\
                 Base()\n\
                 Derived()\n\
                 return Payload(\"OK\").value\n\
             }\n\
         }.read()\n",
    )
    .with_file_stem("InlineLocalConstructors")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "Pass 1 must finalize");
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}
