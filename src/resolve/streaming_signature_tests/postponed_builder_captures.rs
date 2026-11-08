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
fn retained_builder_receiver_capture_publishes_the_completed_builder_solution() {
    // The anonymous object captures the builder receiver while `build`'s `T` is still the
    // postponed call's variable. Pass 1 publishes that capture with the solution the signature
    // graph completed the lambda with, never with the variable or its bound.
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
        .expect("the retained capture must finalize in Pass 1")
        .module
        .index();
    let captures = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("this$receiver"))
        .map(|declaration| {
            index
                .signature(declaration)
                .expect("capture signature")
                .result
                .get()
        })
        .collect::<Vec<_>>();
    assert_eq!(captures, vec![Ty::obj_args("Buildee", &[Ty::String])]);
}
