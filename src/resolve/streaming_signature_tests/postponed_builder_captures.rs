//! Pass-1 signatures of declarations a builder lambda records before its postponed call is
//! solved.

use super::*;

#[test]
fn selected_anonymous_member_effect_constrains_postponed_receiver_lambda() {
    let inputs = [SourceInput::kotlin(
        r#"class Buildee<T : Any> {
            fun consume(value: T) {}
        }
        fun <T : Any> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()
        class Klass {
            val buildee = build {
                object {
                    fun bar() { consume(foo()) }
                    private fun foo() = "OK"
                }.bar()
            }
        }"#,
    )
    .with_file_stem("AnonymousPostponedReceiverSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("anonymous member effects must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("buildee"))
        .and_then(|declaration| index.signature(declaration))
        .expect("buildee signature")
        .result
        .get();
    assert_eq!(result.type_args(), &[Ty::String]);
}

#[test]
fn retained_builder_receiver_capture_records_the_completed_builder_solution() {
    // The anonymous object captures the builder receiver while `build`'s `T` is still the
    // postponed call's variable. The retained analysis records that capture with the solution the
    // signature graph completed the lambda with, never with the variable or its bound. The object
    // is not owned by an inline body, so its capture storage stays out of the Pass-1 index.
    let inputs = [SourceInput::kotlin(
        r#"class Buildee<T : Any> {
            fun consume(value: T) {}
        }
        fun <T : Any> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()
        class Klass {
            val buildee = build {
                object {
                    fun bar() { consume(foo()) }
                    private fun foo() = "OK"
                }.bar()
            }
        }"#,
    )
    .with_file_stem("RetainedBuilderReceiverCapture")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the module must finalize in Pass 1")
        .module
        .index();
    assert_eq!(
        (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .filter(|declaration| index.declaration_name(*declaration) == Some("this$receiver"))
            .count(),
        0,
    );
    let captures = analysis
        .symbols
        .anonymous_object_captures
        .values()
        .flatten()
        .filter(|capture| capture.name == "this$receiver")
        .map(crate::resolve::AnonymousObjectCapture::stored_ty)
        .collect::<Vec<_>>();
    assert_eq!(captures, vec![Ty::obj_args("Buildee", &[Ty::String])]);
}
