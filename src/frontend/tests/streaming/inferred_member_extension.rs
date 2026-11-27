use super::*;

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
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
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
