use super::*;
use crate::backend::{Artifact, CheckedIrFile};
use crate::features::LangFeatures;
use crate::frontend::{analyze_source_set_with_features_and_prepare, CheckedFile};
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;
use crate::types::Ty;

struct SharedClassCaptureBackend;

struct PackageDeclarationBackend;

impl Backend for SharedClassCaptureBackend {
    type State = u8;

    fn lower_file(
        &self,
        _checked: CheckedFile<'_>,
        _stem: &str,
        _state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        panic!("production streaming emission must not invoke legacy syntax lowering")
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if file.ir.shared_class_capture_fields.len() != 1 {
            return Vec::new();
        }
        let (&(class, field), &element) = file
            .ir
            .shared_class_capture_fields
            .iter()
            .next()
            .expect("one shared class capture");
        let declaration = &file.ir.classes[class as usize];
        if element == Ty::String {
            *state |= 1;
        }
        if declaration.is_anonymous_object
            && field == 0
            && declaration.fields[field as usize].ty == Ty::String
            && declaration.ctor_args[field as usize].ty == Ty::String
        {
            *state |= 2;
        }
        if file.ir.exprs.iter().any(|expression| {
            matches!(
                expression,
                crate::ir::IrExpr::New {
                    internal,
                    ctor_params: Some(parameters),
                    ..
                } if *internal == declaration.fq_name_id()
                    && parameters.first() == Some(&Ty::String)
            )
        }) {
            *state |= 4;
        }
        Vec::new()
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![("shared-capture.out".to_string(), vec![state])]
    }
}

impl Backend for PackageDeclarationBackend {
    type State = u8;

    fn lower_file(
        &self,
        _checked: CheckedFile<'_>,
        _stem: &str,
        _state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        panic!("production streaming emission must not invoke legacy syntax lowering")
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let [function] = file.ir.package_functions.as_slice() else {
            return Vec::new();
        };
        if function.name == "pick"
            && function.inline
            && function
                .receiver
                .is_some_and(|receiver| matches!(receiver, Ty::TyParam(..)))
            && function.params == [("value".to_owned(), Ty::String)]
            && function.param_defaults == [true]
            && function.ret == Ty::String
            && function.type_params.len() == 1
            && function.type_params[0].reified
            && !function.spellings.ret.is_none()
            && !function.spellings.param(0).is_none()
        {
            *state |= 1;
        }
        let [property] = file.ir.package_properties.as_slice() else {
            return Vec::new();
        };
        if property.name == "answer"
            && property.ty == Ty::Int
            && property.is_const
            && property.has_constant
            && property.has_backing_field
        {
            *state |= 2;
        }
        let [alias] = file.ir.package_type_aliases.as_slice() else {
            return Vec::new();
        };
        if alias.name == "Label" && alias.expansion == Ty::String && alias.formals.is_empty() {
            *state |= 4;
        }
        Vec::new()
    }

    fn finalize(&self, state: Self::State, _module_name: &str) -> Vec<Artifact> {
        vec![("package-declarations.out".to_string(), vec![state])]
    }
}

#[test]
fn pass_two_publishes_backend_neutral_shared_anonymous_class_capture() {
    let inputs = [SourceInput::kotlin(
        r#"interface Setter { fun set(value: String) }
           fun box(): String {
               var result = "fail"
               val setter = object : Setter {
                   override fun set(value: String) { result = value }
               }
               setter.set("OK")
               return result
           }"#,
    )
    .with_file_stem("SharedAnonymousCapture")];
    let stems = ["SharedAnonymousCapture".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    let artifacts = emit_analyzed(
        analysis,
        &stems,
        &SharedClassCaptureBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(artifacts, [("shared-capture.out".to_string(), vec![7])]);
}

#[test]
fn pass_two_publishes_complete_package_declarations_into_common_ir() {
    let inputs = [SourceInput::kotlin(
        r#"typealias Label = String
           const val answer: Int = 42
           inline fun <reified T> T.pick(value: Label = "x"): Label = value"#,
    )
    .with_file_stem("PackageDeclarations")];
    let stems = ["PackageDeclarations".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    let artifacts = emit_analyzed(
        analysis,
        &stems,
        &PackageDeclarationBackend,
        "main",
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    assert_eq!(
        artifacts,
        [("package-declarations.out".to_string(), vec![7])]
    );
}

#[test]
fn pass_two_preserves_definitely_non_null_generic_parameter_shape() {
    let inputs =
        [
            SourceInput::kotlin("fun <T> f(x: T & Any): T & Any = x\nclass C<T>(val x: T & Any)")
                .with_file_stem("DefinitelyNonNullSignature"),
        ];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize the stable module")
        .module
        .index();
    let source = crate::fir::StreamedModuleSymbols::for_file(index, 0);
    let symbols = crate::symbol_source::SymbolSource::symbols(
        &source,
        crate::symbol_source::SymbolNamespace::Package(crate::types::TypeName::ROOT),
        "f",
    );
    let crate::libraries::Callables::Functions(functions) = &symbols.callables else {
        panic!("f must be projected as a stable function");
    };
    let [function] = functions.overloads.as_slice() else {
        panic!("expected one f overload");
    };
    let signature = function.semantic_signature();
    let [formal] = signature.formals.as_slice() else {
        panic!("f must retain one generic formal");
    };
    let [parameter] = signature.params.as_slice() else {
        panic!("f must retain one parameter");
    };
    let Ty::TyParam(parameter_formal, bound) = parameter else {
        panic!("T & Any must retain its type-parameter identity: {parameter:?}");
    };
    assert_eq!(parameter_formal, formal);
    assert!(!bound.admits_null(), "T & Any must have a non-null bound");
    assert!(
        signature.formal_bounds[0]
            .iter()
            .all(|bound| bound.admits_null()),
        "the declaration formal itself remains nullable"
    );

    let classifier = crate::symbol_source::SymbolSource::symbols(
        &source,
        crate::symbol_source::SymbolNamespace::Package(crate::types::TypeName::ROOT),
        "C",
    )
    .classifier
    .clone()
    .expect("C must be projected as a stable classifier");
    let [constructor] = classifier.constructors.as_slice() else {
        panic!("C must retain one constructor");
    };
    let [parameter] = constructor.params.as_slice() else {
        panic!("C must retain one constructor parameter");
    };
    let Ty::TyParam(_, bound) = parameter else {
        panic!("constructor T & Any must retain its parameter identity: {parameter:?}");
    };
    assert!(
        !bound.admits_null(),
        "constructor T & Any must have a non-null bound"
    );
}

#[test]
fn postponed_builder_receiver_is_read_through_the_anonymous_capture_identity() {
    let inputs = [SourceInput::kotlin(
        r#"fun box(): String = Klass().buildee.produce()

           class Klass {
               val buildee = build {
                   object {
                       fun bar() { consume(foo()) }
                       private fun foo() = "OK"
                   }.bar()
               }
           }

           class Buildee<T : Any> {
               private lateinit var variable: T
               fun consume(arg: T) { variable = arg }
               fun produce(): T = variable
           }

           fun <T : Any> build(instructions: Buildee<T>.() -> Unit): Buildee<T> {
               val result = Buildee<T>()
               instructions(result)
               return result
           }"#,
    )
    .with_file_stem("PostponedBuilderCapture")];
    let stems = ["PostponedBuilderCapture".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    lower_analyzed_to_common_ir(analysis, &stems, "main", &mut diagnostics);

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn generic_context_property_header_uses_its_stable_type_parameter() {
    let source = "// LANGUAGE: +ContextParameters\n\
                  class Result<T>(val value: T)\n\
                  context(result: Result<T>)\n\
                  val <T> current: Result<T> get() = result\n\
                  fun box(): String = \"OK\"\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("GenericContextProperty")];
    let stems = ["GenericContextProperty".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    lower_analyzed_to_common_ir(analysis, &stems, "main", &mut diagnostics);

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn typealiased_fun_interface_constructor_reference_uses_stable_expansion() {
    let source = "fun interface Factory<A, B> { fun invoke(value: A): B }\n\
                  typealias IntFactory<R> = Factory<Int, R>\n\
                  fun accept(factory: ((Int) -> String) -> IntFactory<String>) {}\n\
                  fun box(): String { accept(::IntFactory); return \"OK\" }\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("AliasedFunInterfaceReference")];
    let stems = ["AliasedFunInterfaceReference".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    lower_analyzed_to_common_ir(analysis, &stems, "main", &mut diagnostics);

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn pass_two_publishes_deferred_generic_local_classifier_before_common_lowering() {
    let source = "// LANGUAGE: +NestedTypeAliases +LocalTypeAliases\n\
                  fun box(): String {\n\
                      class Local<T>(val value: T)\n\
                      typealias Alias<T> = Local<T>\n\
                      Alias(\"OK\")\n\
                      return \"OK\"\n\
                  }\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalTypeAlias")];
    let stems = ["LocalTypeAlias".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    lower_analyzed_to_common_ir(analysis, &stems, "main", &mut diagnostics);

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}

#[test]
fn deeply_nested_enum_entry_inner_receiver_reaches_common_lowering() {
    let source = r#"enum class A {
        X {
            val x = "OK"
            inner class Inner {
                inner class Inner2 {
                    inner class Inner3 { val y = x }
                }
            }
            val z = Inner().Inner2().Inner3()
            override val test: String get() = z.y
        };
        abstract val test: String
    }
    fun box() = A.X.test
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DeepEnumEntryInner")];
    let stems = ["DeepEnumEntryInner".to_string()];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);

    lower_analyzed_to_common_ir(analysis, &stems, "main", &mut diagnostics);

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
}
