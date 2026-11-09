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
                    crate::ir::IrExpr::Call { callee, .. }
                        if matches!(**callee, crate::ir::Callee::Module { .. })
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
        if (0..file.ir.functions.len()).any(|function| {
            let function = crate::ir::FunId::try_from(function).expect("a function id fits u32");
            let parameters = file.ir.declared_parameters(function);
            parameters.annotations.iter().any(|annotations| {
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
fn compiler_lowers_a_kotlin_script_as_its_class() {
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

    assert_eq!(emitted_diagnostics(&diags), []);
    assert_eq!(
        outputs
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["Script.out", "module.out"]
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
