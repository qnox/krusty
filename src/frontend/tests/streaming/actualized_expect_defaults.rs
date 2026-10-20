use super::*;

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
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
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
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
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
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
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
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
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
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
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
