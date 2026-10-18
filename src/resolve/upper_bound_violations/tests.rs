use crate::diag::{DiagSink, Severity, Span};
use crate::features::LangFeatures;
use crate::source::SourceInput;
use crate::symbol_source::EmptySymbolSource;

#[test]
fn rejected_alias_keeps_later_bound_diagnostic_in_source_order() {
    let source = r#"// EXPLICIT_API_MODE: STRICT
public open class Bound<T : Number>
typealias MissingVisibility = String
public typealias InvalidBound = Bound<String>
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("MixedAliasDiagnostics")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );
    let _ = crate::compiler::check_frontend_only(analysis, &mut diagnostics);

    let missing = u32::try_from(
        source
            .find("typealias MissingVisibility")
            .expect("missing-visibility alias"),
    )
    .expect("test source offset fits u32");
    let invalid = u32::try_from(source.rfind("String").expect("invalid bound argument"))
        .expect("test source offset fits u32");
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| (
                diagnostic.file,
                diagnostic.span,
                diagnostic.severity,
                diagnostic.msg.as_str(),
            ))
            .collect::<Vec<_>>(),
        [
            (
                0,
                Span::new(missing, missing + 27),
                Severity::Error,
                "visibility must be specified in explicit API mode.",
            ),
            (
                0,
                Span::new(invalid, invalid + 6),
                Severity::Error,
                "type argument is not within its bounds: type parameter 'T (of class Bound<T : Number>)' must be subtype of 'Number', but actual: 'String'.",
            ),
        ]
    );
}
