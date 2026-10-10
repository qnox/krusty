use super::*;
use crate::backend::Artifact;
use crate::compilation_target::CompilationTarget;
use crate::features::LangFeatures;
use crate::frontend::analyze_source_set_with_features;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;
use crate::types::Ty;

fn initialized_jvm_libraries(
    classpath: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::jvm_libraries::JvmLibraries {
    crate::jvm::jvm_libraries::JvmLibraries::new(classpath).expect("JVM provider initialization")
}

fn emitted_diagnostics(diags: &DiagSink) -> Vec<(u32, &str)> {
    diags
        .diags
        .iter()
        .map(|diagnostic| (diagnostic.file, diagnostic.msg.as_str()))
        .collect()
}

/// Every artifact path an emission produced, in emission order.
fn artifact_paths(outputs: &[Artifact]) -> Vec<&str> {
    outputs.iter().map(|(path, _)| path.as_str()).collect()
}

struct RecordingBackend;

struct ModuleCallRecordingBackend;

struct FileAnnotationRecordingBackend;

struct MemberAnnotationRecordingBackend;

struct BodylessClassAnnotationRecordingBackend;

struct EnumEntryCallRecordingBackend;

impl Backend for RecordingBackend {
    type State = usize;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        CompilationTarget::Jvm
    }

    fn lower_ir_file(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        *state += file
            .ir
            .functions
            .iter()
            .filter(|function| function.body.is_some())
            .count();
        let stem = &file.stems[file.source.raw() as usize];
        vec![(format!("{stem}.out"), Vec::new())]
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![("module.out".to_string(), state.to_string().into_bytes())]
    }
}

impl Backend for ModuleCallRecordingBackend {
    type State = usize;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        CompilationTarget::Jvm
    }

    fn lower_ir_file(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        *state += file
            .ir
            .exprs
            .iter()
            .filter(|expression| {
                matches!(
                    expression,
                    crate::ir::IrExpr::Call {
                        callee: crate::ir::Callee::Module { .. },
                        ..
                    }
                )
            })
            .count();
        Vec::new()
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![(
            "module-calls.out".to_string(),
            state.to_string().into_bytes(),
        )]
    }
}

impl Backend for FileAnnotationRecordingBackend {
    type State = usize;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        CompilationTarget::Jvm
    }

    fn lower_ir_file(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        *state += file.ir.file_annotations.iter().len();
        Vec::new()
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![(
            "file-annotations.out".to_string(),
            state.to_string().into_bytes(),
        )]
    }
}

impl Backend for MemberAnnotationRecordingBackend {
    type State = usize;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        CompilationTarget::Jvm
    }

    fn lower_ir_file(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if file.ir.function_annotations.values().any(|annotations| {
            annotations
                .applications()
                .any(|annotation| annotation.internal.matches("Marker"))
        }) {
            *state |= 1;
        }
        if file.ir.fn_param_annotations.values().any(|parameters| {
            parameters.iter().any(|annotations| {
                annotations
                    .applications()
                    .any(|annotation| annotation.internal.matches("ParameterMarker"))
            })
        }) {
            *state |= 2;
        }
        Vec::new()
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![(
            "member-annotations.out".to_string(),
            state.to_string().into_bytes(),
        )]
    }
}

impl Backend for BodylessClassAnnotationRecordingBackend {
    type State = usize;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        CompilationTarget::Jvm
    }

    fn lower_ir_file(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if file.ir.classes.iter().any(|class| {
            class.fq_name.matches("Bodyless")
                && class
                    .applied_annotations
                    .applications()
                    .any(|annotation| annotation.internal.matches("Marker"))
        }) {
            *state += 1;
        }
        Vec::new()
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![(
            "bodyless-class-annotations.out".to_string(),
            state.to_string().into_bytes(),
        )]
    }
}

impl Backend for EnumEntryCallRecordingBackend {
    type State = usize;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        CompilationTarget::Jvm
    }

    fn lower_ir_file(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if file
            .ir
            .classes
            .iter()
            .any(|class| class.fq_name.matches("Foo$FOO"))
        {
            *state |= 1;
        }
        if file
            .ir
            .exprs
            .iter()
            .any(|expression| matches!(expression, crate::ir::IrExpr::MethodCall { .. }))
        {
            *state |= 2;
        }
        if file
            .ir
            .exprs
            .iter()
            .all(|expression| !matches!(expression, crate::ir::IrExpr::Checked(_)))
        {
            *state |= 4;
        }
        Vec::new()
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![(
            "enum-entry-calls.out".to_string(),
            state.to_string().into_bytes(),
        )]
    }
}

/// The target a source set is analyzed for and the target its backend emits for are one
/// value: a program checked under one target's source rules never reaches another target's
/// backend, whichever entry path built it.
#[test]
fn a_backend_refuses_a_source_set_analyzed_for_another_target() {
    let inputs = [SourceInput::kotlin("value class V(val x: Int)\n").with_file_stem("V")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::new(
            CompilationTarget::Native,
            Box::new(EmptySymbolSource),
        ),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(emitted_diagnostics(&diagnostics), Vec::new());

    let artifacts = emit_analyzed(
        analysis,
        &["V".to_string()],
        &RecordingBackend,
        "module",
        &mut diagnostics,
    );

    assert_eq!(artifacts, Vec::new());
    assert_eq!(
        emitted_diagnostics(&diagnostics),
        vec![(
            0,
            "internal error: the source set was analyzed for Native but the backend emits for Jvm"
        )]
    );
}

#[test]
fn inline_local_class_capture_context_survives_the_pass_two_reparse() {
    let inputs = [SourceInput::kotlin(
        r#"inline fun <T> once(block: () -> T): T = block()
               inline fun host(crossinline callback: () -> Int): Int {
                   var outside = 0
                   return once {
                       var inside = 0
                       val holder = object {
                           fun count() { outside++; inside++ }
                           fun value(): Int = callback()
                       }
                       holder.count()
                       holder.value() + outside + inside
                   }
               }"#,
    )
    .with_file_stem("InlineLocalClassCapture")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(
        census.is_conformant(),
        "Pass-2 nested bodies must receive Pass-1 checked capture context: {:?}",
        census.failures
    );
}

#[test]
fn anonymous_object_captures_outer_receiver_used_inside_a_local_function() {
    let inputs = [SourceInput::kotlin(
        r#"class Host {
                   fun outer() {
                       val instance = object {
                           fun invoke() {
                               fun nested() { target() }
                               nested()
                           }
                       }
                       instance.invoke()
                   }
                   fun target() {}
               }"#,
    )
    .with_file_stem("AnonymousOuterReceiver")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn anonymous_method_demands_later_inferred_member_in_the_same_pass_two_group() {
    let inputs = [SourceInput::kotlin(
        r#"fun box(): String {
                   val prefix = "a"
                   val value = object {
                       override fun toString(): String = foo(prefix) + foo("b")
                       fun foo(value: String) = value + value
                   }
                   return value.toString()
               }"#,
    )
    .with_file_stem("AnonymousForwardMember")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn production_frontend_selects_applied_concrete_super_property_over_abstract_builtin() {
    let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() else {
        return;
    };
    let inputs = [SourceInput::kotlin(
            r#"interface DefaultSize<T> {
                   val size: T get() = 56 as T
               }
               class Values : Collection<String>, DefaultSize<Int> {
                   override fun isEmpty() = throw UnsupportedOperationException()
                   override fun contains(value: String) = throw UnsupportedOperationException()
                   override fun iterator() = throw UnsupportedOperationException()
                   override fun containsAll(values: Collection<String>) = throw UnsupportedOperationException()
                   override val size: Int get() = super.size
               }"#,
        )
        .with_file_stem("AppliedSuperProperty")];
    let mut paths = vec![stdlib];
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(classpath))),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn bounded_local_classifier_does_not_steal_nested_classifier_identity() {
    let inputs = [SourceInput::kotlin(
        r#"class BeforeA
               class BeforeB
               class Outer {
                   class NestedA
                   class NestedB
                   fun make(): Any {
                       class Local
                       return Local()
                   }
               }"#,
    )
    .with_file_stem("BoundedLocalClassifierIdentity")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn bounded_local_constructor_does_not_republish_top_level_constructor_shape() {
    let inputs = [SourceInput::kotlin(
        r#"class Box<T>(val value: T)
               fun <F> enterLocal(box: Box<F>) {
                   class Local<L>(value: L)
                   Local(box.value)
               }
               fun box(): String = Box("OK").value"#,
    )
    .with_file_stem("BoundedLocalConstructorIdentity")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn bounded_top_level_unit_property_is_visible_to_later_units() {
    let inputs = [SourceInput::kotlin(
        r#"val first: Unit get() {}
               val second = first
               fun use(): Unit {
                   first
                   second
               }"#,
    )
    .with_file_stem("TopLevelUnitProperty")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn pass_two_checks_inferred_extension_body_against_its_stable_result() {
    let inputs = [SourceInput::kotlin(
        r#"class Wrap<T>(val value: T)
               interface Consumer<T> { fun consume(value: T): String }
               open class Base<T> : Consumer<Wrap<T>> {
                   override fun consume(value: Wrap<T>): String = "OK"
               }
               class Derived : Base<String>()
               fun <T> Consumer<Wrap<T>>.adapt(value: T) = consume(Wrap(value))
               fun box(): String = Derived().adapt("OK")"#,
    )
    .with_file_stem("StableExtensionResult")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn covariant_override_keeps_inherited_default_provider_and_derived_result() {
    let inputs = [SourceInput::kotlin(
        r#"open class Base {
                   open fun value(input: Any = "OK"): Any = input
               }
               class Derived : Base() {
                   override fun value(input: Any): String = "OK"
               }
               fun box(): String = Derived().value()"#,
    )
    .with_file_stem("InheritedOverrideDefault")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(
        analysis
            .streamed
            .as_ref()
            .expect("the two-pass frontend must finalize")
            .module
            .default_arguments()
            .len(),
        2,
        "the base declaration and its overriding call target share checked default payload"
    );
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn anonymous_super_constructor_uses_canonical_active_classifier() {
    let inputs = [SourceInput::kotlin(
        r#"abstract class Base {
                   abstract fun value(): String
               }
               fun box(): String {
                   var result = "fail"
                   val instance = object : Base() {
                       override fun value(): String {
                           result = "OK"
                           return result
                       }
                   }
                   return instance.value()
               }"#,
    )
    .with_file_stem("CanonicalAnonymousClassifier")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn anonymous_member_signature_keeps_outer_type_parameter_identity() {
    let inputs = [SourceInput::kotlin(
        r#"interface Cursor<T>
               interface Stream<T> {
                   fun iterator(): Cursor<T>
               }
               class ZippingStream<T1, T2>(
                   val stream1: Stream<T1>,
                   val stream2: Stream<T2>,
               ) {
                   fun iterator(): Any = object {
                       val iterator1 = stream1.iterator()
                       val iterator2 = stream2.iterator()
                   }
               }
               object EmptyCursor : Cursor<Nothing>
               object EmptyStream : Stream<Nothing> {
                   override fun iterator(): Cursor<Nothing> = EmptyCursor
               }
               fun consume() {
                   ZippingStream(EmptyStream, EmptyStream)
               }"#,
    )
    .with_file_stem("AnonymousOuterTypeParameterIdentity")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn nested_member_of_local_class_keeps_classifier_owner_and_captures() {
    let inputs = [SourceInput::kotlin(
        r#"open class Base(val read: () -> String)
               fun box(): String {
                   val prefix = "P"
                   class Local(val value: String) {
                       inner class Inner : Base({ value }) {
                           fun current(): String = prefix + read()
                       }
                   }
                   return Local("OK").Inner().current()
               }"#,
    )
    .with_file_stem("NestedLocalMemberOwner")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn anonymous_initializer_lambda_captures_outer_receiver_and_constructor_value() {
    let inputs = [SourceInput::kotlin(
        r#"fun doSomething(block: () -> Unit) { block() }
               class Host(result: String) {
                   init {
                       val holder = object {
                           init { doSomething { completed(result) } }
                       }
                   }
                   fun completed(value: String) {}
               }"#,
    )
    .with_file_stem("AnonymousInitializerCapture")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn selected_primary_constructor_checks_singleton_default() {
    let inputs = [SourceInput::kotlin(
        r#"object Empty
               open class Base(val context: Any = Empty)
               class Derived : Base()
               fun box(): String = if (Derived().context === Empty) "OK" else "BAD""#,
    )
    .with_file_stem("PrimaryConstructorDefault")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn production_stream_keeps_explicit_backing_field_storage_distinct() {
    let inputs = [SourceInput::kotlin(
        r#"// LANGUAGE: +ExplicitBackingFields
               interface View
               class Stored(val value: Int) : View
               class Holder {
                   val item: View
                       field = Stored(42)
                   fun read(): Int = item.value
               }
               fun box(): String = if (Holder().read() == 42) "OK" else "BAD""#,
    )
    .with_file_stem("ExplicitBackingField")];
    let stems = ["ExplicitBackingField".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &RecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(!outputs.is_empty());
}

#[test]
fn bounded_local_signature_publication_uses_the_exact_classifier_root() {
    let inputs = [SourceInput::kotlin(
        r#"interface Callback { fun invoke(): String }
               open class Base(val callback: Callback)
               class Outer {
                   val ok = "OK"
                   inner class Inner : Base(object : Callback {
                       override fun invoke() = ok
                   })
               }
               fun box(): String = Outer().Inner().callback.invoke()"#,
    )
    .with_file_stem("NestedLocalSignatures")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn deep_anonymous_signature_dependencies_share_one_body_group() {
    let inputs = [SourceInput::kotlin(
        r#"interface Result { fun value(): String }
               class Host(val text: String) {
                   fun outer() = object : Result {
                       fun middle() = object : Result {
                           fun inner() = object : Result {
                               val captured = text
                               override fun value() = captured
                           }
                           override fun value() = inner().value()
                       }
                       override fun value() = middle().value()
                   }
               }
               fun box() = Host("OK").outer().value()"#,
    )
    .with_file_stem("DeepAnonymousSignatures")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn pending_free_local_callable_header_is_available_to_earlier_body_group() {
    let inputs = [SourceInput::kotlin(
        r#"class Outer {
                   private companion object { val result = "OK" }
                   class Nested {
                       fun make() = object {
                           override fun toString(): String = result
                       }
                   }
                   fun read() = Nested().make().toString()
               }
               fun box() = Outer().read()"#,
    )
    .with_file_stem("EarlyLocalCallableHeader")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn production_emission_requires_and_consumes_finalized_pass_one() {
    let inputs = [SourceInput::kotlin("fun box(): String = \"OK\"")];
    let stems = ["Main".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &RecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(outputs[0].0, "Main.out");
    assert_eq!(outputs[1], ("module.out".to_string(), b"1".to_vec()));
}

#[test]
fn production_stream_checks_file_annotations_before_common_lowering() {
    let inputs = [SourceInput::kotlin(
        r#"@file:Marker("header")
               annotation class Marker(val value: String)
               fun box(): String = "OK""#,
    )
    .with_file_stem("CheckedFileAnnotation")];
    let stems = ["CheckedFileAnnotation".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &FileAnnotationRecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(
        outputs,
        [("file-annotations.out".to_string(), b"1".to_vec())]
    );
}

#[test]
fn production_stream_attaches_checked_member_and_parameter_annotations_by_stable_identity() {
    let inputs = [SourceInput::kotlin(
        r#"annotation class Marker
               annotation class ParameterMarker
               class Host {
                   @Marker
                   fun value(@ParameterMarker input: Int): Int = input
               }"#,
    )
    .with_file_stem("CheckedMemberAnnotations")];
    let stems = ["CheckedMemberAnnotations".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &MemberAnnotationRecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(
        outputs,
        [("member-annotations.out".to_string(), b"3".to_vec())]
    );
}

#[test]
fn production_stream_checks_metadata_for_a_classifier_without_a_body() {
    let inputs = [SourceInput::kotlin(
        r#"annotation class Marker
               @Marker interface Bodyless"#,
    )
    .with_file_stem("BodylessClassAnnotation")];
    let stems = ["BodylessClassAnnotation".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &BodylessClassAnnotationRecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(
        outputs,
        [("bodyless-class-annotations.out".to_string(), b"1".to_vec())]
    );
}

#[test]
fn production_stream_materializes_enum_entry_private_member_calls() {
    let source = r#"// WITH_STDLIB
            enum class Foo {
                FOO {
                    private fun privateBar() = "bar"
                    override fun bar(): String = privateBar()
                    override fun foo(): String = "foo"
                    override var xxx: String
                        get() = "xxx"
                        set(value: String) {}
                };
                abstract fun foo(): String
                abstract fun bar(): String
                abstract var xxx: String
            }
        "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("EnumEntryCalls")];
    let stems = ["EnumEntryCalls".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(classpath))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &EnumEntryCallRecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(
        outputs,
        [("enum-entry-calls.out".to_string(), b"7".to_vec())]
    );
}

#[test]
fn production_stream_lowers_anonymous_member_capture_of_extension_receiver() {
    let inputs = [SourceInput::kotlin(
        r#"interface TextView<T> { val value: T }
               fun String.view() = object : TextView<String> {
                   override val value: String = length.toString()
               }
               class Host {
                   fun String.sizeText() = this.length
                   fun String.nestedView() = object : TextView<String> {
                       override val value: String = sizeText().toString()
                   }
                   fun String.explicitView() = object : TextView<String> {
                       override val value: String = "123".sizeText().toString()
                   }
                   fun read(text: String): String = text.nestedView().value
                   fun readExplicit(text: String): String = text.explicitView().value
               }
               fun box(): String {
                   if ("OK".view().value != "2") return "BAD-1"
                   if (Host().read("OK") != "2") return "BAD-2"
                   return if (Host().readExplicit("OK") == "3") "OK" else "BAD-3"
               }"#,
    )
    .with_file_stem("CapturedExtensionReceiver")];
    let stems = ["CapturedExtensionReceiver".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(classpath))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &RecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(!outputs.is_empty());
}

#[test]
fn production_stream_lowers_nested_local_extension_receiver_captures() {
    let inputs = [SourceInput::kotlin(
        r#"fun <T> eval(fn: () -> T) = fn()
               fun String.f(x: String): String {
                   fun String.g() = eval { this@f + this@g }
                   return x.g()
               }
               fun box() = "O".f("K")"#,
    )
    .with_file_stem("NestedLocalExtensionReceivers")];
    let stems = ["NestedLocalExtensionReceivers".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(classpath))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &RecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(!outputs.is_empty());
}

#[test]
fn production_stream_lowers_structural_receiver_captures_through_class_storage() {
    let inputs = [
        SourceInput::kotlin(
            r#"class Host {
                   val suffix = "K"
                   fun String.local(): String {
                       class Local {
                           fun result() = this@local + this@Host.suffix
                       }
                       return Local().result()
                   }

                   fun callLocal() = "O".local()
               }

               fun localBox() = Host().callLocal()"#,
        )
        .with_file_stem("StructuralReceiverCaptures"),
        SourceInput::kotlin(
            r#"open class Base(val callback: () -> String)
               class Outer {
                   val ok = "OK"
                   inner class Inner : Base({
                       val nested = { ok }
                       nested()
                   })
               }
               fun constructorBox() = Outer().Inner().callback()"#,
        )
        .with_file_stem("ConstructorReceiverCaptures"),
    ];
    let stems = [
        "StructuralReceiverCaptures".to_string(),
        "ConstructorReceiverCaptures".to_string(),
    ];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(classpath))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &RecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(!outputs.is_empty());
}

#[test]
fn production_stream_lowers_extension_property_receiver_in_context_function() {
    let inputs = [SourceInput::kotlin(
        r#"class C(var a: String) { fun foo(): String = a }
               val C.y
                   get() = context(x: C) fun (): String { return this@y.foo() + x.foo() }
               fun consume(x: context(C) () -> String): String = x(C("K"))
               fun box(): String = consume(C::y.get(C("O")))"#,
    )
    .with_file_stem("ExtensionPropertyContextFunction")];
    let stems = ["ExtensionPropertyContextFunction".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(classpath))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &RecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(!outputs.is_empty());
}

#[test]
fn production_emission_never_lowers_without_finalized_signatures() {
    let inputs = [SourceInput::kotlin("fun a() = b()\nfun b() = a()")];
    let stems = ["Cycle".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &RecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(outputs.is_empty());
    assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| (diagnostic.file, diagnostic.span, diagnostic.msg.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    0,
                    crate::diag::Span::new(10, 13),
                    "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.",
                ),
                (
                    0,
                    crate::diag::Span::new(24, 27),
                    "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.",
                ),
            ]
        );
}

#[test]
fn production_stream_preserves_cross_file_calls_by_stable_module_identity() {
    let inputs = [
        SourceInput::kotlin("fun answer(): Int = helper()").with_file_stem("Caller"),
        SourceInput::kotlin("fun helper(): Int = 42").with_file_stem("Helper"),
    ];
    let stems = ["Caller".to_string(), "Helper".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &ModuleCallRecordingBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(outputs, [("module-calls.out".to_string(), b"1".to_vec())]);
}

#[test]
fn jvm_production_stream_realizes_cross_file_module_calls_after_fir() {
    let inputs = [
        SourceInput::kotlin("fun answer(): Int = helper()").with_file_stem("Caller"),
        SourceInput::kotlin("fun helper(): Int = 42").with_file_stem("Helper"),
    ];
    let stems = ["Caller".to_string(), "Helper".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs.iter().any(|(path, _)| path == "CallerKt.class"));
    assert!(outputs.iter().any(|(path, _)| path == "HelperKt.class"));
}

#[test]
fn jvm_production_stream_realizes_external_calls_by_provider_identity() {
    let inputs = [SourceInput::kotlin(
        r#"fun box(): String {
                val member = "abc".substring(1)
                val extension = "x".repeat(2)
                val length = "abc".length
                val indices = "abc".indices
                val builder = StringBuilder("abc")
                println(member)
                if (length != 3) return "BAD"
                if (indices.first != 0) return "BAD"
                if (builder.toString() != "abc") return "BAD"
                return member + extension
            }"#,
    )
    .with_file_stem("ExternalCalls")];
    let stems = ["ExternalCalls".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "ExternalCallsKt.class"));
}

#[test]
fn jvm_production_stream_materializes_source_property_storage() {
    let inputs = [SourceInput::kotlin(
        r#"val top: Int = 2

            class Box {
                val value: Int = 1
            }

            fun box(): String = if (top + Box().value == 3) "OK" else "BAD""#,
    )
    .with_file_stem("SourceProperties")];
    let stems = ["SourceProperties".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "SourcePropertiesKt.class"));
    assert!(outputs.iter().any(|(path, _)| path == "Box.class"));
}

#[test]
fn pass_one_publishes_cross_file_primary_constructor_identity() {
    let inputs = [
        SourceInput::kotlin("fun use(): Holder = Holder(7)").with_file_stem("Use"),
        SourceInput::kotlin("class Holder(val value: Int)").with_file_stem("Holder"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 module")
        .module
        .index();
    let constructor = index
        .constructor_declaration(crate::types::type_name("Holder"), true, &[Ty::Int])
        .expect("stable primary constructor");
    assert!(index.callable_for_declaration(constructor).is_some());
}

#[test]
fn jvm_production_stream_realizes_cross_file_properties_and_constructors() {
    let inputs = [
        SourceInput::kotlin(
            "fun box(): String = if (answer == 42 && Holder(7).value == 7) \"OK\" else \"BAD\"",
        )
        .with_file_stem("Use"),
        SourceInput::kotlin("val answer: Int = 42\nclass Holder(val value: Int)")
            .with_file_stem("Declarations"),
    ];
    let stems = ["Use".to_string(), "Declarations".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs.iter().any(|(path, _)| path == "UseKt.class"));
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "DeclarationsKt.class"));
    assert!(outputs.iter().any(|(path, _)| path == "Holder.class"));
}

#[test]
fn jvm_production_stream_realizes_checked_sam_lambda() {
    let inputs = [SourceInput::kotlin(
        r#"fun interface Action {
                fun run(value: Int): String
            }

            fun consume(action: Action): String = action.run(42)
            fun box(): String = consume { value -> if (value == 42) "OK" else "BAD" }"#,
    )
    .with_file_stem("SamLambda")];
    let stems = ["SamLambda".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs.iter().any(|(path, _)| path == "SamLambdaKt.class"));
    assert!(outputs.iter().any(|(path, _)| path == "Action.class"));
}

#[test]
fn jvm_production_stream_realizes_source_object_receiver() {
    let inputs = [SourceInput::kotlin(
        r#"object Values {
                val answer: Int = 42
            }

            fun box(): String = if (Values.answer == 42) "OK" else "BAD""#,
    )
    .with_file_stem("ObjectReceiver")];
    let stems = ["ObjectReceiver".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "ObjectReceiverKt.class"));
    assert!(outputs.iter().any(|(path, _)| path == "Values.class"));
}

#[test]
fn jvm_production_stream_realizes_cross_file_object_receiver() {
    let inputs = [
        SourceInput::kotlin("fun box(): String = if (Values.answer == 42) \"OK\" else \"BAD\"")
            .with_file_stem("UseObject"),
        SourceInput::kotlin("object Values { val answer: Int = 42 }")
            .with_file_stem("ObjectDeclaration"),
    ];
    let stems = ["UseObject".to_string(), "ObjectDeclaration".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs.iter().any(|(path, _)| path == "UseObjectKt.class"));
    assert!(outputs.iter().any(|(path, _)| path == "Values.class"));
}

#[test]
fn jvm_production_stream_realizes_checked_class_literals() {
    let inputs = [SourceInput::kotlin(
        r#"fun literals(value: String): String {
                val unbound = String::class
                val bound = value::class
                return if (unbound == bound) "OK" else "BAD"
            }
            fun inferredUnbound() = String::class
            fun inferredBound(value: String) = value::class
            fun inferredPrimitive() = Int::class
            fun explicitPrimitive(): kotlin.reflect.KClass<Int> = Int::class
            val topLevelValue: String = "value"
            fun inferredTopLevelBound() = topLevelValue::class
            class LiteralHolder(val value: String)
            class SelfLiteral { fun inferredSelf() = this::class }
            val topLevelHolder: LiteralHolder = LiteralHolder("value")
            fun inferredQualifiedBound() = topLevelHolder.value::class"#,
    )
    .with_file_stem("ClassLiterals")];
    let stems = ["ClassLiterals".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("class literal signatures must finalize before Pass 2");
    let signature = |name: &str| {
        (0..streamed.module.index().declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find_map(|declaration| {
                (streamed.module.index().declaration_name(declaration) == Some(name))
                    .then(|| streamed.module.index().signature(declaration))
                    .flatten()
            })
            .expect("named class-literal declaration")
            .result
            .get()
    };
    assert_eq!(
        signature("inferredPrimitive"),
        signature("explicitPrimitive")
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "ClassLiteralsKt.class"));
}

#[test]
fn jvm_production_stream_realizes_source_iterator_loop() {
    let inputs = [SourceInput::kotlin(
        r#"class WordsIterator {
                operator fun hasNext(): Boolean = false
                operator fun next(): String = "word"
            }
            class Words { operator fun iterator(): WordsIterator = WordsIterator() }
            fun consume(words: Words) { for (word in words) { word } }"#,
    )
    .with_file_stem("IteratorLoop")];
    let stems = ["IteratorLoop".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "IteratorLoopKt.class"));
}

#[test]
fn jvm_production_stream_realizes_dependency_iterator_loop() {
    let inputs = [SourceInput::kotlin(
        r#"fun consume(values: List<Int>): Int {
                var total = 0
                for (value in values) total += value
                return total
            }"#,
    )
    .with_file_stem("DependencyIterator")];
    let stems = ["DependencyIterator".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "DependencyIteratorKt.class"));
}

#[test]
fn jvm_production_stream_realizes_source_callable_references() {
    let inputs = [SourceInput::kotlin(
            r#"fun increment(value: Int): Int = value + 1
            fun withDefault(value: Int, amount: Int = 1): Int = value + amount
            fun sum(vararg values: Int): Int = values[0] + values[1]
            fun <T> identity(value: T): T = value
            fun String.append(value: String): String = this + value
            class Counter { fun increment(value: Int): Int = value + 1 }
            fun references(counter: Counter): Int {
                val topLevel = ::increment
                val bound = counter::increment
                val unbound = Counter::increment
                val defaulted: (Int) -> Int = ::withDefault
                val packed: (Int, Int) -> Int = ::sum
                val generic: (String) -> String = ::identity
                val boundExtension = "a"::append
                val unboundExtension = String::append
                return topLevel(1) + bound(1) + unbound(counter, 1) + defaulted(1) + packed(1, 2) + generic("x").length + boundExtension("b").length + unboundExtension("a", "b").length
            }"#,
        )
        .with_file_stem("SourceReferences")];
    let stems = ["SourceReferences".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "SourceReferencesKt.class"));
}

#[test]
fn jvm_production_stream_realizes_cross_file_callable_reference() {
    let inputs = [
        SourceInput::kotlin("fun referenced(value: Int): Int = value + 1")
            .with_file_stem("ReferenceTarget"),
        SourceInput::kotlin(
            "fun invokeReference(): Int { val reference = ::referenced; return reference(41) }",
        )
        .with_file_stem("ReferenceCaller"),
    ];
    let stems = ["ReferenceTarget".to_string(), "ReferenceCaller".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "ReferenceTargetKt.class"));
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "ReferenceCallerKt.class"));
}

#[test]
fn jvm_production_stream_realizes_capturing_local_function_reference() {
    let inputs = [SourceInput::kotlin(
        r#"fun apply(value: Int, operation: (Int) -> Int): Int = operation(value)

            fun box(): String {
                val base = 40
                fun add(value: Int): Int = base + value
                return if (apply(2, ::add) == 42) "OK" else "BAD"
            }"#,
    )
    .with_file_stem("LocalReference")];
    let stems = ["LocalReference".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "LocalReferenceKt.class"));
}

#[test]
fn jvm_production_stream_realizes_local_defaults_and_adapted_reference() {
    let inputs = [SourceInput::kotlin(
        r#"enum class FirChoice { OK }

            fun suspendReference(): suspend (Int) -> Int {
                fun increment(value: Int): Int = value + 1
                return ::increment
            }

            fun box(): String {
                suspendReference()
                val prefix = ""
                fun join(value: String = prefix, suffix: String = "K"): String = value + suffix
                fun sum(vararg values: Int): Int = values[0] + values[1]
                fun String.tag(suffix: String): String = this + suffix
                val reference: (String) -> String = ::join
                val sumReference: (Int, Int) -> Int = ::sum
                val tagReference: (String) -> String = "O"::tag
                val dependencyReference: (String) -> String = String::uppercase
                val boundDependencyReference: () -> String = "ok"::uppercase
                val topLevelDependencyReference: () -> List<String> = ::emptyList
                val adaptedDependencyReference: (String, Int) -> String = String::padEnd
                val boundAdaptedDependencyReference: (Int) -> String = "x"::padEnd
                val varargDependencyReference: (String, String) -> List<String> = ::listOf
                val suspendDependencyReference: suspend (String) -> String = String::uppercase
                val propertyDependencyReference: (String) -> Int = String::length
                val boundPropertyDependencyReference: () -> Int = "OK"::length
                val reflectivePropertyDependencyReference = String::length
                val boundReflectivePropertyDependencyReference = "OK"::length
                val classifierPropertyReference = FirChoice::entries
                return if (sum(1, 2) == 3 && sumReference(1, 2) == 3 &&
                    "O".tag("K") == "OK" && tagReference("K") == "OK" &&
                    dependencyReference("ok") == "OK" && boundDependencyReference() == "OK" &&
                    topLevelDependencyReference().isEmpty() &&
                    adaptedDependencyReference("x", 2) == "x " &&
                    boundAdaptedDependencyReference(2) == "x " &&
                    varargDependencyReference("O", "K").size == 2 &&
                    propertyDependencyReference("OK") == 2 &&
                    boundPropertyDependencyReference() == 2 &&
                    reflectivePropertyDependencyReference.get("OK") == 2 &&
                    boundReflectivePropertyDependencyReference.get() == 2 &&
                    classifierPropertyReference.get()[0].name == "OK" &&
                    suspendDependencyReference.toString().isNotEmpty()) {
                    join(suffix = "K") + reference("")
                } else "BAD"
            }"#,
    )
    .with_file_stem("LocalDefaults")];
    let stems = ["LocalDefaults".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "LocalDefaultsKt.class"));
}

#[test]
fn jvm_production_stream_realizes_local_delegated_property() {
    let inputs = [SourceInput::kotlin(
        r#"class Delegate {
                operator fun getValue(owner: Any?, property: Any?): String = "OK"
                operator fun setValue(owner: Any?, property: Any?, value: String) {}
            }

            fun box(): String {
                var value by Delegate()
                value = "OK"
                return value
            }"#,
    )
    .with_file_stem("LocalDelegate")];
    let stems = ["LocalDelegate".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert!(outputs
        .iter()
        .any(|(path, _)| path == "LocalDelegateKt.class"));
}

#[test]
fn jvm_production_stream_realizes_non_capturing_local_class() {
    let inputs = [SourceInput::kotlin(
        r#"fun box(): String {
                class Local(val value: String) {
                    fun read(): String = value
                }
                return Local("OK").read()
            }"#,
    )
    .with_file_stem("LocalClassifier")];
    let stems = ["LocalClassifier".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert_eq!(emitted_diagnostics(&diagnostics), Vec::new());
    assert_eq!(
        artifact_paths(&outputs),
        [
            "LocalClassifierKt.class",
            "LocalClassifierKt$box$Local.class",
            "META-INF/main.kotlin_module"
        ]
    );
}

#[test]
fn jvm_production_stream_realizes_capturing_local_class() {
    let inputs = [SourceInput::kotlin(
        r#"class Outer(val prefix: String) {
                fun result(): String {
                    val suffix = "K"
                    class Local(val own: String) {
                        fun read(): String {
                            fun local(): String {
                                class Nested {
                                    fun value(): String = suffix
                                }
                                return Nested().value()
                            }
                            return prefix + local() + own
                        }
                    }
                    return Local("").read()
                }
            }

            fun nested(): String {
                val make = {
                    val prefix = "O"
                    class Nested {
                        fun read(): String = prefix + "K"
                    }
                    Nested().read()
                }
                return make()
            }

            fun box(): String = Outer("O").result() + nested()"#,
    )
    .with_file_stem("CapturingLocalClassifier")];
    let stems = ["CapturingLocalClassifier".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert_eq!(emitted_diagnostics(&diagnostics), Vec::new());
    assert_eq!(
        artifact_paths(&outputs),
        [
            "CapturingLocalClassifierKt.class",
            "Outer.class",
            "Outer$result$Local.class",
            "Outer$result$Local$read$local$Nested.class",
            "CapturingLocalClassifierKt$nested$make$1$Nested.class",
            "META-INF/main.kotlin_module"
        ]
    );
}

#[test]
fn jvm_production_stream_publishes_inherited_local_class_members() {
    let inputs = [SourceInput::kotlin(
        r#"fun hierarchy(captured: String): String {
                open class Local {
                    fun value() = captured
                }
                open class Derived : Local() {
                    fun inherited() = value()
                }
                return Derived().inherited()
            }

            fun box(): String = hierarchy("OK")"#,
    )
    .with_file_stem("LocalHierarchy")];
    let stems = ["LocalHierarchy".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert_eq!(emitted_diagnostics(&diagnostics), Vec::new());
    assert_eq!(
        artifact_paths(&outputs),
        [
            "LocalHierarchyKt.class",
            "LocalHierarchyKt$hierarchy$Derived.class",
            "LocalHierarchyKt$hierarchy$Local.class",
            "META-INF/main.kotlin_module"
        ]
    );
}

#[test]
fn jvm_production_stream_resolves_self_instantiation_from_inner_local_class() {
    let inputs = [SourceInput::kotlin(
        r#"fun box(): String {
                val capturedInConstructor = 1
                val capturedInBody = 10
                class C(var x: Int) {
                    var y = 0

                    inner class D {
                        fun copyOuter(): C {
                            val result = C(x)
                            result.y += capturedInBody
                            return result
                        }
                    }

                    init {
                        y += x + capturedInConstructor
                    }
                }

                val result = C(100).D().copyOuter()
                return if (result.x == 100 && result.y == 111) "OK" else "fail"
            }"#,
    )
    .with_file_stem("LocalSelfInstantiation")];
    let stems = ["LocalSelfInstantiation".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = crate::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert_eq!(emitted_diagnostics(&diagnostics), Vec::new());
    assert_eq!(
        artifact_paths(&outputs),
        [
            "LocalSelfInstantiationKt.class",
            "LocalSelfInstantiationKt$box$C.class",
            "LocalSelfInstantiationKt$box$C$D.class",
            "META-INF/main.kotlin_module"
        ]
    );
}

#[test]
fn compiler_orchestrates_frontend_then_backend() {
    let mut diags = DiagSink::new();
    let inputs = [SourceInput::kotlin("fun box(): String = \"OK\"").with_file_stem("Main")];
    let stems = vec!["Main".to_string()];
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diags,
    );
    let outputs = emit_analyzed(analysis, &stems, &RecordingBackend, "main", &mut diags);

    assert!(!diags.has_errors(), "{:?}", diags.diags);
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].0, "Main.out");
    assert_eq!(outputs[1], ("module.out".to_string(), b"1".to_vec()));
}

#[test]
fn compiler_does_not_lower_after_frontend_error() {
    let mut diags = DiagSink::new();
    let inputs = [SourceInput::kotlin("fun box(): Int = \"no\"").with_file_stem("Main")];
    let stems = vec!["Main".to_string()];
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diags,
    );
    let outputs = emit_analyzed(analysis, &stems, &RecordingBackend, "main", &mut diags);

    assert_eq!(
        emitted_diagnostics(&diags),
        [(0, "return type mismatch: expected 'Int', actual 'String'.")]
    );
    assert!(outputs.is_empty());
}

#[test]
fn oversized_conflicting_overload_signature_still_blocks_lowering() {
    let parameter = "value".repeat(14 * 1024);
    let source = format!("fun crowded({parameter}: Int): Int = 0");
    let mut diags = DiagSink::new();
    let inputs = [
        SourceInput::kotlin(&source).with_file_stem("First"),
        SourceInput::kotlin(&source).with_file_stem("Second"),
    ];
    let stems = vec!["First".to_string(), "Second".to_string()];
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diags,
    );
    let outputs = emit_analyzed(analysis, &stems, &RecordingBackend, "main", &mut diags);

    assert_eq!(
        emitted_diagnostics(&diags),
        [(0, "conflicting overloads:"), (1, "conflicting overloads:"),]
    );
    assert!(outputs.is_empty());
}

#[test]
fn same_file_private_and_public_signature_conflict_blocks_lowering() {
    let source = "fun crowded(value: Int): Int = value\n\
                      private fun crowded(value: Int): Int = value";
    let mut diags = DiagSink::new();
    let inputs = [SourceInput::kotlin(source).with_file_stem("Main")];
    let stems = vec!["Main".to_string()];
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diags,
    );
    let outputs = emit_analyzed(analysis, &stems, &RecordingBackend, "main", &mut diags);

    assert_eq!(
        emitted_diagnostics(&diags),
        [
            (0, "conflicting overloads:\nfun crowded(value: Int): Int"),
            (0, "conflicting overloads:\nfun crowded(value: Int): Int"),
        ]
    );
    assert!(outputs.is_empty());
}

#[test]
fn cross_file_private_context_function_cannot_reach_lowering() {
    let mut diags = DiagSink::new();
    let inputs = [
        SourceInput::kotlin(
            "fun <T, R> with(receiver: T, block: T.() -> R): R = receiver.block()\n\
                 class Scope\n\
                 fun use(scope: Scope): Int = with(scope) { hidden(1) }",
        )
        .with_file_stem("Caller"),
        SourceInput::kotlin("private context(scope: Scope) fun hidden(value: Int): Int = value")
            .with_file_stem("Hidden"),
    ];
    let stems = vec!["Caller".to_string(), "Hidden".to_string()];
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diags,
    );
    let outputs = emit_analyzed(analysis, &stems, &RecordingBackend, "main", &mut diags);

    assert_eq!(
        emitted_diagnostics(&diags),
        [(0, "cannot access 'hidden': it is private in its file")]
    );
    assert!(outputs.is_empty());
}

#[test]
fn compiler_does_not_emit_kotlin_scripts() {
    let source = "val value = 1";
    let mut diags = DiagSink::new();
    let inputs = [SourceInput::kotlin_script(source).with_file_stem("Script")];
    let stems = vec!["Script".to_string()];
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diags,
    );
    let outputs = emit_analyzed(analysis, &stems, &RecordingBackend, "main", &mut diags);

    assert!(outputs.is_empty());
    assert_eq!(
        emitted_diagnostics(&diags),
        [(0, "Kotlin scripts can be analyzed but cannot be emitted")]
    );
}

#[test]
fn streamed_emission_rejects_misaligned_source_metadata() {
    let mut diags = DiagSink::new();
    let inputs = [SourceInput::kotlin("fun box(): String = \"OK\"").with_file_stem("Main")];
    let analysis = analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diags,
    );
    let outputs = emit_analyzed(analysis, &[], &RecordingBackend, "main", &mut diags);

    assert!(outputs.is_empty());
    assert_eq!(
        emitted_diagnostics(&diags),
        [(
            0,
            "internal error: source files, stems, and checked types have different lengths"
        )]
    );
}
