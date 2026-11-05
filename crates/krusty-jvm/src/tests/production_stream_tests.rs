//! The JVM production stream end to end: checked FIR through common lowering into this backend.

use krusty::backend::Artifact;
use krusty::compiler::*;
use krusty::diag::DiagSink;
use krusty::features::LangFeatures;
use krusty::source::SourceInput;

fn initialized_jvm_libraries(
    classpath: std::rc::Rc<krusty::jvm::classpath::Classpath>,
) -> krusty::jvm::jvm_libraries::JvmLibraries {
    krusty::jvm::jvm_libraries::JvmLibraries::new(classpath).expect("JVM provider initialization")
}

/// The emitted diagnostics as `(file, message)` pairs, in emission order.
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

#[test]
fn jvm_production_stream_realizes_cross_file_module_calls_after_fir() {
    let inputs = [
        SourceInput::kotlin("fun answer(): Int = helper()").with_file_stem("Caller"),
        SourceInput::kotlin("fun helper(): Int = 42").with_file_stem("Helper"),
    ];
    let stems = ["Caller".to_string(), "Helper".to_string()];
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let signature = |name: &str| {
        analysis
            .finalized_signature(name)
            .expect("class literal signatures must finalize before Pass 2")
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
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
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
fn jvm_production_stream_captures_anonymous_receiver_in_accessor_lambda() {
    let inputs = [SourceInput::kotlin(
        r#"fun build(): Int {
            var count = 0
            val holder = object {
                val action: () -> Unit get() = { count++ }
            }
            holder.action()
            return count
        }

        fun box(): String = if (build() == 1) "OK" else "fail""#,
    )
    .with_file_stem("AnonymousAccessorCapture")];
    let stems = ["AnonymousAccessorCapture".to_string()];
    let mut paths = Vec::new();
    if let Some(stdlib) = krusty::jvm::kotlin_stdlib_jar() {
        paths.push(stdlib);
    }
    if let Some(jdk) = krusty::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        krusty::frontend::PlatformProvider::jvm(Box::new(initialized_jvm_libraries(
            classpath.clone(),
        ))),
        &LangFeatures::new(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    let outputs = emit_analyzed(
        analysis,
        &stems,
        &crate::JvmBackend::new(classpath),
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(
        outputs
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([
            "AnonymousAccessorCaptureKt.class",
            "AnonymousAccessorCaptureKt$build$holder$1.class",
            "META-INF/main.kotlin_module",
        ])
    );
}
