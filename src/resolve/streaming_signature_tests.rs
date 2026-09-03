use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;
use crate::types::Ty;

fn stable_declaration_at(
    analysis: &crate::frontend::SourceSetAnalysis,
    span: crate::diag::Span,
    kind: crate::fir::DeclarationKind,
) -> crate::fir::DeclarationId {
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let source = crate::fir::SourceFileId::from_raw(0);
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        source,
        index,
    )
    .expect("the retained test AST must bind to the stable declaration stream");
    (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| anchor.kind == kind)
                && active.span(&analysis.files[0], *declaration) == Some(span)
        })
        .max_by_key(|declaration| index.declaration_header(*declaration).is_some())
        .expect("stable declaration at active span")
}

#[test]
fn generated_data_class_copy_publishes_its_complete_callable_header() {
    let source = "data class Pair(val left: Int, val right: String)";
    let inputs = [SourceInput::kotlin(source).with_file_stem("GeneratedDataCopy")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let provisional_copy = analysis
        .symbols
        .class_by_type_name(crate::types::type_name("Pair"))
        .and_then(|class| class.methods.get("copy"))
        .and_then(|overloads| overloads.first())
        .expect("provisional generated copy signature");
    assert_eq!(provisional_copy.params.len(), 2);
    let index = analysis
        .streamed
        .as_ref()
        .expect("generated copy signature must finalize")
        .module
        .index();
    let copy = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index.declaration_name(*declaration) == Some("copy")
                && index
                    .declaration_header(*declaration)
                    .is_some_and(|header| {
                        header
                            .flags
                            .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
                    })
        })
        .expect("stable generated copy declaration");
    let signature = index
        .signature(copy)
        .expect("resolved generated copy signature");
    assert_eq!(signature.parameters.len(), 2);
    let callable = index
        .callable_for_declaration(copy)
        .expect("generated copy callable header");
    assert_eq!(index.callable_parameter_name_count(callable.id), 2);
    for (ordinal, expected) in ["left", "right"].into_iter().enumerate() {
        assert_eq!(
            index.callable_parameter_name(callable.id, ordinal as u32),
            Some(expected)
        );
        assert!(index
            .callable_parameter(callable.id, ordinal as u32)
            .expect("generated copy parameter")
            .flags()
            .has_default());
    }
}

#[test]
fn implicit_any_is_the_class_super_rung_during_signature_solving() {
    let source = r#"
interface Contract {
    override fun equals(other: Any?): Boolean
    override fun hashCode(): Int
}
class Implementation : Contract {
    override fun equals(other: Any?) = super.equals(other)
    override fun hashCode() = super.hashCode()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ImplicitAnySuper")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("super calls must produce a finalized signature module")
        .module
        .index();
    let owner = index
        .classifier_declaration(crate::types::type_name("Implementation"))
        .expect("implementation declaration");
    for (name, result) in [("equals", Ty::Boolean), ("hashCode", Ty::Int)] {
        let declaration = index
            .declarations_named(name)
            .iter()
            .copied()
            .find(|declaration| {
                index
                    .declaration_header(*declaration)
                    .is_some_and(|header| header.owner == Some(owner))
            })
            .expect("implementation member declaration");
        assert_eq!(
            index
                .signature(declaration)
                .map(|signature| signature.result.get()),
            Some(result)
        );
    }
}

#[test]
fn compact_classifier_publication_does_not_restore_a_validated_cycle_edge() {
    let source = "object Cyclic : Cyclic()";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Cyclic")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(
        diagnostics
            .diags
            .iter()
            .any(|diagnostic| diagnostic.msg.contains("cycle in supertypes")),
        "the invalid edge must still be diagnosed: {:?}",
        diagnostics.diags
    );
    assert!(
        analysis.streamed.is_some(),
        "unrelated signatures remain publishable"
    );
    let classifier = analysis
        .symbols
        .class_by_type_name(crate::types::type_name("Cyclic"))
        .expect("source classifier");
    assert_eq!(classifier.super_internal, None);
    assert!(classifier.interfaces.is_empty());
}

#[test]
fn singleton_interface_edge_survives_compact_signature_finalization() {
    let source = r#"
interface A
object O : A
fun consume(value: A): A = value
fun test(): A = consume(O)
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("SingletonInterfaceEdge")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let singleton = analysis
        .symbols
        .class_by_type_name(crate::types::type_name("O"))
        .expect("singleton signature");
    assert!(
        singleton
            .interfaces
            .contains_name(crate::types::type_name("A")),
        "singleton lost its semantic interface edge: {:?}",
        singleton.interfaces
    );
}

#[test]
fn large_arity_typealias_does_not_replace_a_sibling_classifier_header() {
    let parameters = std::iter::repeat_n("A", 24)
        .chain(std::iter::once("T"))
        .collect::<Vec<_>>()
        .join(", ");
    let arguments = std::iter::repeat_n("O", 24)
        .chain(std::iter::once("\"OK\""))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "// LANGUAGE: +FunctionTypesWithBigArity\n\
         interface A\n\
         object O : A\n\
         typealias F<T> = ({parameters}) -> String\n\
         fun test(f: F<String>): String = f({arguments})\n"
    );
    let inputs = [SourceInput::kotlin(&source).with_file_stem("LargeAritySiblingHeader")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(&source),
        &mut diagnostics,
    );

    let singleton = analysis
        .symbols
        .class_by_type_name(crate::types::type_name("O"))
        .expect("singleton signature");
    assert_eq!(
        singleton.interfaces.iter_ids().collect::<Vec<_>>(),
        vec![crate::types::type_name("A")]
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn inferred_member_signature_resolves_its_lexically_nested_classifier() {
    let source = r#"
class Outer {
    class Nested(val value: Int)
    fun nested() = Nested(1)
}
fun box() = Outer().nested().value
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedSignatureType")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "lexically nested signature types must finalize before body checking"
    );
}

#[test]
fn contextual_lambda_in_inferred_member_resolves_lexically_nested_constructor() {
    let source = r#"
fun <T> build(init: (Int) -> T): T = init(0)

class Outer {
    class Nested(val value: Int)
    fun nested() = build { Nested(it) }
}

fun box() = Outer().nested().value
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedContextualSignatureType")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a postponed lambda must use the lexical nested-classifier rung while its expected shape is materialized"
    );
}

#[test]
fn inferred_member_binds_inherited_inner_constructor_to_implicit_receiver() {
    let source = r#"
open class Base {
    inner class Nested(val value: String)
}

class Derived : Base() {
    fun nested() = Nested("OK")
}

fun box() = Derived().nested().value
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("InheritedInnerSignatureType")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "an inherited inner constructor must be selected from the implicit receiver before Pass 2"
    );
}

#[test]
fn inferred_enum_companion_members_use_outer_enum_lexical_scope() {
    let source = r#"
enum class Game {
    ROCK,
    PAPER;

    companion object {
        fun first() = ROCK
        val all = values()
        val second = valueOf("PAPER")
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("EnumCompanionSignatureScope")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "enum entries and synthetic enum callables must remain visible through the stable owner chain"
    );
}

#[test]
fn inferred_member_invokes_callable_member_property_on_same_receiver_rung() {
    let source = r#"
class Holder(val value: (() -> String) -> String) {
    fun result() = value { "OK" }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("CallableMemberSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a callable-valued member property must shape and resolve invoke at its receiver rung"
    );
}

#[test]
fn typed_call_reaches_member_extension_rung_after_expectation_materialization() {
    let source = r#"
class Receiver

fun consume(block: Receiver.() -> Unit) {}

class Scope {
    fun Receiver.extension(value: String) {}
    fun inferred() = consume { extension("OK") }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("TypedMemberExtensionSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "typed arguments need no provisional candidate lookup before final tower selection"
    );
}

#[test]
fn qualified_call_uses_source_typealias_classifier_coordinate() {
    let source = r#"
class Owner {
    companion object {
        fun value() = "OK"
    }
}

typealias Alias = Owner
fun inferred() = Alias.value()
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("QualifiedAliasSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a qualified typealias call must select through its expanded classifier coordinate"
    );
}

#[test]
fn local_classifier_supertype_resolves_a_lexical_sibling() {
    let source = r#"
fun <T> value(): Any {
    open class Base<X>
    class Derived : Base<String>()
    return Derived()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalSiblingSupertype")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a local classifier supertype must resolve in its declaration's lexical scope"
    );
}

#[test]
fn classifier_header_does_not_see_its_own_nested_declarations() {
    let source = r#"
package second

interface Base<A> { fun foo(): String = "OK" }

class MyClass(val prop: second.Base<second.Base<Int>>) : Base<Base<Int>> by prop {
    interface Base
}

fun box(): String {
    val data = MyClass(object : Base<Base<Int>> {})
    return data.foo()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ClassifierHeaderScope")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a classifier header must resolve before declarations from its body enter scope"
    );
}

#[test]
fn contextual_constructor_reference_finalizes_through_classifier_declaration() {
    let source = r#"
class Value(val text: String)
fun interface Factory { fun make(text: String): Value }
fun make(factory: Factory) = factory.make("OK")
fun box() = make(::Value).text
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ConstructorReferenceSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a contextual constructor reference must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved box signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn java_sam_context_binds_a_generic_inner_constructor_reference() {
    let kotlin = r#"// WITH_STDLIB
class Outer<TO>(val first: TO) {
    inner class Inner<TI>(val second: TI) {
        fun text() = first.toString() + second.toString()
    }
}
fun box() = Sam(Outer("O")::Inner).get("K").text()
"#;
    let inputs = [
        SourceInput::kotlin(kotlin).with_file_stem("GenericInnerConstructorReference"),
        SourceInput::java(
            "public interface Sam<TO, TI> { Outer<TO>.Inner<TI> get(String value); }",
        )
        .with_file_stem("Sam"),
    ];
    let mut paths = crate::toolchain::classpath_jars_for(kotlin);
    paths.push(
        crate::toolchain::jdk_modules()
            .expect("Java SAM inner-constructor regression requires the configured JDK"),
    );
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the Java SAM must finalize the generic inner constructor reference")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved box signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn sam_constructor_contextually_checks_array_constructor_reference() {
    let source = r#"// WITH_STDLIB
fun interface Sam { fun make(size: Int): IntArray }
fun array(): IntArray = Sam(::IntArray).make(2)
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("SamArrayConstructorReference")];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the SAM constructor must supply its method shape before checking the reference")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("array"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved array signature");
    assert_eq!(result.result.get(), Ty::obj("kotlin/IntArray"));
}

#[test]
fn provider_member_contextually_checks_callable_reference_before_diagnostics() {
    let source = r#"// WITH_STDLIB
fun merge(map: java.util.HashMap<String, Int>) = map.merge("a", 2, Int::plus)
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ProviderMemberCallableReference")];
    let mut paths = crate::toolchain::classpath_jars_for(source);
    paths.push(
        crate::toolchain::jdk_modules()
            .expect("provider member callable-reference regression requires the configured JDK"),
    );
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis
            .symbols
            .funs
            .get("merge")
            .and_then(|overloads| overloads.first())
            .is_some_and(|signature| signature.ret != Ty::Error),
        "the provider-member call must produce a checked source signature"
    );
}

#[test]
fn callable_reference_retains_nothing_as_a_bottom_result() {
    let source = r#"// WITH_STDLIB
fun interface BottomSam { fun get(value: String): Any }
fun thr(value: String): Nothing = throw RuntimeException(value)
fun adapt(): BottomSam = BottomSam(::thr)
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NothingCallableReference")];
    let mut paths = crate::toolchain::classpath_jars_for(source);
    paths.push(
        crate::toolchain::jdk_modules()
            .expect("Nothing callable-reference regression requires the configured JDK"),
    );
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a Nothing-returning reference must finalize through ordinary bottom subtyping")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("adapt"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved adapt signature");
    assert_eq!(result.result.get(), Ty::obj("BottomSam"));
}

#[test]
fn contextual_unbound_member_reference_demands_the_source_result() {
    let kotlin = r#"
class Value(val text: String) { fun read() = text }
fun box() = Reader(Value::read)
"#;
    let inputs = [
        SourceInput::java("public interface Reader { String read(Value value); }")
            .with_file_stem("Reader"),
        SourceInput::kotlin(kotlin).with_file_stem("MemberReferenceSignature"),
    ];
    let stems = ["Reader".to_string(), "MemberReferenceSignature".to_string()];
    let mut paths = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    paths.push(
        crate::toolchain::jdk_modules()
            .expect("Java SAM member-reference regression requires the configured JDK"),
    );
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("an unbound member reference must demand its inferred source result")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved box signature");
    assert_eq!(result.result.get(), Ty::obj("Reader"));
}

#[test]
fn inferred_signature_applies_explicit_type_arguments_to_demanded_member_results() {
    let source = r#"// WITH_STDLIB
class Scope {
    inline fun <reified T : Any> of() = T::class
}
class Outer {
    fun <R : Any> choose(block: Scope.() -> R): R = Scope().block()
}
fun <R : Any> outer(block: Outer.() -> R): R = Outer().block()
class Result
class App {
    val type = outer { choose { of<Result>() } }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ExplicitMemberTypeArgument")];
    let stems = ["ExplicitMemberTypeArgument".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("explicit generic member call must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("type"))
        .and_then(|declaration| index.signature(declaration))
        .expect("finalized App.type signature")
        .result
        .get();
    assert_eq!(result.type_args(), &[Ty::obj("Result")], "{result:?}");
}

#[test]
fn inferred_signature_resolves_a_private_nested_classifier_star_import() {
    let source = r#"
package test
import test.A.B.*
class A {
    private class B { class D }
    fun make() = D()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedClassifierStarSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("nested classifier star import must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("make"))
        .and_then(|declaration| index.signature(declaration))
        .expect("make signature");
    assert_eq!(result.result.get(), Ty::obj("test/A$B$D"));
}

#[test]
fn inferred_signature_resolves_an_explicitly_imported_enum_entry() {
    let source = r#"
package test
import test.Choice.ONE
private enum class Choice { ONE, TWO }
fun chosen() = ONE
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ImportedEnumEntrySignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("explicit enum entry import must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("chosen"))
        .and_then(|declaration| index.signature(declaration))
        .expect("chosen signature");
    assert_eq!(result.result.get(), Ty::obj("test/Choice"));
}

#[test]
fn inferred_explicit_backing_field_uses_public_type_as_generic_expectation() {
    let source = "// LANGUAGE: +ExplicitBackingFields\n// WITH_STDLIB\n\
        val items: List<String>\n    field = mutableListOf()\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("GenericBackingFieldSignature")];
    let mut classpath = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        )),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("backing-field signature must finalize in Pass 1")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("items"))
        .expect("items declaration");
    let storage = index
        .property_for_declaration(declaration)
        .and_then(|property| index.property(property))
        .and_then(|property| property.storage_type)
        .expect("checked backing-field storage type");
    assert_eq!(
        storage.get(),
        Ty::obj_args("kotlin/collections/MutableList", &[Ty::String])
    );
}

#[test]
fn declared_explicit_backing_field_publishes_its_storage_type() {
    let source = r#"// LANGUAGE: +ExplicitBackingFields
class Derived {
    val value: Any
        field: String = "OK"

    fun usage(): String = value
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DeclaredBackingFieldSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("declared backing-field signature must finalize in Pass 1")
        .module
        .index();
    let storage = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("value"))
        .find_map(|declaration| {
            index
                .property_for_declaration(declaration)
                .and_then(|property| index.property(property))
                .and_then(|property| property.storage_type)
        })
        .expect("declared backing-field storage type");
    assert_eq!(storage.get(), Ty::String);
}

#[test]
fn inferred_signature_resolves_a_package_qualified_nested_constructor() {
    let source = r#"
package Package
class Outer {
    class Nested {
        val first = "O"
        val second = "K"
    }
}
fun box() = Package.Outer.Nested().first + Outer.Nested().second
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("QualifiedNestedSignature")];
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        )),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("package-qualified nested construction must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved box signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn inferred_member_signature_resolves_invoke_on_this() {
    let inputs = [SourceInput::kotlin(
        "class Callable {\n\
             operator fun invoke() = 42\n\
             fun result() = this()\n\
         }\n",
    )
    .with_file_stem("InvokeOnThisSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("invoke on the stable dispatch receiver must finalize")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved result signature");
    assert_eq!(result.result.get(), Ty::Int);
}

#[test]
fn nested_object_signature_sees_enclosing_object_members() {
    let source = r#"
class Host {
    fun result() = Outer.Inner.call() + Outer.Inner.value

    private object Outer {
        fun text() = "O"
        val suffix = "K"

        object Inner {
            fun call() = text()
            val value = suffix
        }
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedObjectSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "an enclosing object is a lexical singleton receiver during signature solving"
    );
}

#[test]
fn inferred_signature_keeps_anonymous_type_while_consuming_its_local_member() {
    let inputs = [SourceInput::kotlin(
        "open class Base\n\
         fun result() = object : Base() { fun local() = \"OK\" }.local()\n",
    )
    .with_file_stem("AnonymousIntermediateSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("an anonymous intermediate value must finalize through its local member")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved result signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn inferred_signature_demands_member_of_anonymous_object_property() {
    let inputs = [SourceInput::kotlin(
        "val LocalClass = object { override fun toString() = \"OK\" }\n\
         fun result() = LocalClass.toString()\n",
    )
    .with_file_stem("AnonymousPropertyMemberSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("an inferred anonymous-object member must finalize on demand")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved result signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn enclosing_signature_does_not_duplicate_an_eager_local_member_constraint() {
    let source = r#"
class Holder {
    val values = buildList {
        class Local {
            val effect = { add("OK") }
        }
        Local().effect()
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedLocalConstraint")];
    let mut diagnostics = DiagSink::new();
    let platform = crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(crate::toolchain::classpath_jars_for(
            "// WITH_STDLIB",
        )),
    ));
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(platform),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the local member constraint discovered from the enclosing property must be reused"
    );
}

#[test]
fn inferred_anonymous_member_sees_its_enclosing_dispatch_receiver() {
    let inputs = [SourceInput::kotlin(
        "class Owner {\n\
             fun result() = object { override fun toString() = value() }.toString()\n\
             fun value() = \"OK\"\n\
         }\n",
    )
    .with_file_stem("AnonymousMemberDispatchReceiver")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a captured dispatch receiver must finalize in the compact graph")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved member signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn ordinary_getter_backing_field_is_visible_to_nested_anonymous_signature() {
    let source = r#"
abstract class Base {
    abstract val value: String
    fun read() = value
}
val result: String = "O"
    get() = object : Base() {
        override val value = field
    }.read() + "K"
fun box() = result
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("GetterFieldAnonymousSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the nested anonymous property must bind the enclosing semantic backing-field symbol in Pass 1"
    );
}

#[test]
fn local_super_argument_anonymous_signature_captures_enclosing_value() {
    let source = r#"
interface Callback { fun invoke(): String }
open class Base(val callback: Callback)
fun box(): String {
    val expected = "OK"
    class Local : Base(object : Callback {
        override fun invoke() = expected
    })
    return Local().callback.invoke()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalSuperCaptureSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "an anonymous member in local super arguments must finalize from its enclosing lexical capture"
    );
}

#[test]
fn nested_anonymous_signature_reaches_the_nominal_outer_receiver_chain() {
    let source = r#"
interface Callback { fun invoke(): String }
open class Base(val callback: Callback)
class Outer {
    val expected = "OK"
    inner class Inner : Base(object : Callback {
        override fun invoke() = (object : Callback {
            override fun invoke() = expected
        }).invoke()
    })
}
fun box(): String = Outer().Inner().callback.invoke()
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedAnonymousOuterSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "nested anonymous members must retain every local rung before the nominal outer receiver chain"
    );
}

#[test]
fn local_member_signature_captures_the_semantic_for_range_element() {
    let source = r#"
fun box(): Int {
    for (element in 0 .. 1) {
        class Local { fun value() = element }
        return Local().value()
    }
    return 0
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ForRangeLocalSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a local member in a for body must capture the checked loop-element type during Pass 1"
    );
}

#[test]
fn nested_classifier_of_local_class_keeps_the_enclosing_function_scope() {
    let source = r#"
fun value(expected: String): String {
    class Local {
        inner class Nested { fun result() = expected }
    }
    return Local().Nested().result()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedLocalCaptureSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a nested classifier of a local class must finalize under the original lexical function scope"
    );
}

#[test]
fn inferred_local_subclass_member_demands_its_inherited_local_signature() {
    let source = r#"
fun value(expected: String): String {
    open class Base { fun inherited() = expected }
    class Derived : Base() { fun result() = inherited() }
    return Derived().result()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalHierarchySignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "an inherited inferred member of a local superclass must be demanded by stable declaration identity"
    );
}

#[test]
fn inferred_local_super_call_demands_the_selected_stable_member() {
    let source = r#"
fun value(expected: String): String {
    open class Base { open fun result() = expected }
    class Derived : Base() { override fun result() = super.result() }
    return Derived().result()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalSuperSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a local super call must demand its selected inferred member by stable declaration identity"
    );
}

#[test]
fn nested_classifier_resolves_its_enclosing_local_classifier_type() {
    let source = r#"
fun box(): Any {
    class Local {
        inner class Nested {
            fun copyOuter(): Local = Local()
        }
    }
    return Local().Nested().copyOuter()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedLocalTypeSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "a classifier nested in a local class must bind the enclosing local classifier by stable type identity"
    );
}

#[test]
fn local_delegated_supertype_publishes_captured_semantic_formal() {
    let source = r#"
interface Callback { fun foo() }
interface SuccessCallback<in T> { fun onSuccess(value: T) }
interface MaybeCallbacks<in T> : SuccessCallback<T>, Callback
interface Disposable
interface Observer<in T> { fun onSubscribe(disposable: Disposable) }
interface MaybeObserver<in T> : Observer<T>, MaybeCallbacks<T>

fun <T> test(emitter: MaybeCallbacks<T>) {
    class OuterLocal<U> : MaybeObserver<T>, Callback by emitter {
        override fun onSubscribe(disposable: Disposable) {}
        override fun onSuccess(value: T) {
            class InnerLocal : MaybeObserver<T>, Observer<T> by this,
                MaybeCallbacks<T> by emitter
        }
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalDelegatedSupertype")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("captured delegated supertypes must finalize in Pass 1")
        .module
        .index();
    let classifier = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter_map(|declaration| index.classifier_header(declaration))
        .find(|classifier| classifier.classifier.contains("InnerLocal"))
        .expect("stable InnerLocal classifier header");
    assert!(classifier
        .interfaces
        .iter()
        .all(|interface| !interface.get().mentions_error()));
    assert!(classifier.interfaces.iter().all(|interface| {
        interface
            .get()
            .type_args()
            .first()
            .is_some_and(|argument| argument.mentions_ty_param())
    }));
}

#[test]
fn nested_anonymous_delegation_publishes_only_finalized_classifier_hierarchies() {
    let source = r#"
interface Callback { fun foo() }
interface SuccessCallback<in T> { fun onSuccess(value: T) }
interface MaybeCallbacks<in T> : SuccessCallback<T>, Callback
interface Disposable
interface Observer { fun onSubscribe(disposable: Disposable) }
interface MaybeObserver<in T> : Observer, MaybeCallbacks<T>

fun <T, R> test(emitter: MaybeCallbacks<R>) {
    object : MaybeObserver<T>, Callback by emitter {
        override fun onSubscribe(disposable: Disposable) {}
        override fun onSuccess(value: T) {
            object : MaybeObserver<R>, Observer by this, MaybeCallbacks<R> by emitter {}
        }
    }
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedAnonymousDelegation")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("nested anonymous delegation must finalize in Pass 1")
        .module
        .index();
    for raw in 0..index.declaration_count() {
        let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
        let Some(hierarchy) = index.classifier_hierarchy(declaration) else {
            continue;
        };
        assert!(hierarchy.iter().all(|parent| {
            !parent.applied.get().mentions_error() && !parent.applied.get().mentions_pending()
        }));
    }
}

#[test]
fn function_supertype_hierarchy_keeps_its_semantic_shape_during_projection() {
    let source = r#"// WITH_STDLIB
// LANGUAGE: +SuspendFunctionAsSupertype
class Callable<T> : suspend (T) -> String {
    override suspend fun invoke(value: T): String = "OK"
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("FunctionSupertypeHierarchy")];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::from_source(source),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("function supertypes must survive finalized metadata projection")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|header| header.classifier.matches("Callable"))
        })
        .expect("stable Callable classifier");
    let hierarchy = index
        .classifier_hierarchy(declaration)
        .expect("published Callable hierarchy");
    let header = index
        .classifier_header(declaration)
        .expect("stable Callable header");
    assert!(header
        .interfaces
        .iter()
        .any(|parent| matches!(parent.get(), Ty::Fun(_))));
    assert!(hierarchy.iter().all(|parent| {
        !parent.applied.get().mentions_error() && !parent.applied.get().mentions_pending()
    }));
}

#[test]
fn interface_delegation_specializes_a_finalized_inferred_member_result() {
    let source = r#"
interface Source<T> {
    val stored: T
    fun value() = stored
}
class Delegate(override val stored: String) : Source<String>
class Wrapper(delegate: Source<String>) : Source<String> by delegate
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DelegatedInferredResult")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("delegation must close from finalized Pass-1 signatures")
        .module
        .index();
    let wrapper = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter_map(|declaration| index.classifier_header(declaration))
        .find(|classifier| classifier.classifier.contains("Wrapper"))
        .expect("stable Wrapper classifier header");
    let value = wrapper
        .interface_delegations
        .iter()
        .flat_map(|delegation| delegation.members.iter())
        .find_map(|member| match member {
            crate::fir::ResolvedDelegatedMember::Function(function)
                if function.name.as_ref() == "value" =>
            {
                Some(&function.call)
            }
            _ => None,
        })
        .expect("selected value forwarding declaration");
    assert_eq!(value.result.get(), Ty::String);
    assert!(!value.result.get().mentions_pending());
}

#[test]
fn interface_delegation_uses_finalized_inferred_property_and_function_results() {
    let source = r#"
fun propertyValue(): String = "O"
fun functionValue(): String = "K"
interface A {
    val property get() = propertyValue()
    fun function() = functionValue()
}
class B(val delegate: A) : A by delegate
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DelegatedInferredMembers")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("delegation must close from finalized Pass-1 signatures")
        .module
        .index();
    let wrapper = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter_map(|declaration| index.classifier_header(declaration))
        .find(|classifier| classifier.classifier.matches("B"))
        .expect("stable B classifier header");
    let members = wrapper
        .interface_delegations
        .first()
        .expect("stable interface-delegation plan")
        .members
        .as_ref();
    let property = members
        .iter()
        .find_map(|member| match member {
            crate::fir::ResolvedDelegatedMember::Property(property)
                if property.name.as_ref() == "property" =>
            {
                Some(property)
            }
            _ => None,
        })
        .expect("delegated inferred property");
    let function = members
        .iter()
        .find_map(|member| match member {
            crate::fir::ResolvedDelegatedMember::Function(function)
                if function.name.as_ref() == "function" =>
            {
                Some(function)
            }
            _ => None,
        })
        .expect("delegated inferred function");

    assert_eq!(property.ty.get(), Ty::String);
    assert_eq!(property.getter.result.get(), Ty::String);
    assert_eq!(function.call.result.get(), Ty::String);
}

#[test]
fn inferred_extension_property_sees_its_context_receiver() {
    let source = r#"// LANGUAGE: +ContextReceivers
class View { val coefficient = 42 }
context(View) val Int.dp get() = coefficient
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ContextPropertySignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the context property signature must finalize")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("dp"))
        .expect("stable dp property declaration");
    let signature = index
        .signature(declaration)
        .expect("finalized dp property signature");
    let property = index
        .property_for_declaration(declaration)
        .and_then(|property| index.property(property))
        .expect("stable dp property shape");

    assert_eq!(signature.parameters.len(), 1);
    assert_eq!(signature.parameters[0].get(), Ty::obj("View"));
    assert_eq!(signature.result.get(), Ty::Int);
    assert_eq!(property.context_parameter_count, 1);
    assert_eq!(
        property.extension_receiver.map(|ty| ty.get()),
        Some(Ty::Int)
    );
}

#[test]
fn explicit_generic_anonymous_override_does_not_poison_property_inference() {
    let source = r#"
interface Key<T, R>
interface ErrorTest { fun <T : Key<T, R>, R> get(key: T): R? }
val errorTest = object : ErrorTest {
    override fun <T : Key<T, R>, R> get(key: T): R? = null
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("GenericAnonymousOverride")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("an explicit generic override must not block Pass-1 finalization")
        .module
        .index();
    let property = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("errorTest"))
        .and_then(|declaration| index.signature(declaration))
        .expect("stable errorTest signature");
    assert!(property
        .result
        .get()
        .obj_internal()
        .is_some_and(|classifier| classifier.matches("ErrorTest")));
}

#[test]
fn local_classifier_header_resolves_statement_local_type_alias() {
    let source = r#"
abstract class A { abstract val p: String }
fun make(): A {
    typealias Text = String
    class B(override val p: Text) : A()
    return B("OK")
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalAliasHeader")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("ordinary local declarations must not block Pass-1 finalization")
        .module
        .index();
    let classifier = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter_map(|declaration| index.classifier_header(declaration))
        .find(|classifier| classifier.classifier.contains("$B"))
        .expect("stable local B classifier");
    let constructor = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.owner == Some(classifier.declaration)
                        && header.kind == crate::fir::DeclarationKind::Constructor
                })
        })
        .expect("stable B constructor declaration");
    assert!(
        index.signature(constructor).is_none(),
        "an ordinary local constructor header is Pass-2 lexical work"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn anonymous_classifier_header_expands_generic_statement_local_type_alias() {
    let source = r#"
open class Generic<K>
fun make(): Any {
    typealias Alias<K> = Generic<K>
    return object : Alias<String>() {}
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("GenericLocalAliasHeader")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("an undemanded generic local alias must not block Pass-1 finalization")
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
        .expect("stable anonymous declaration header");
    assert!(
        index.classifier_header(anonymous).is_none(),
        "the ordinary anonymous classifier's lexical parent belongs to Pass 2"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn secondary_constructor_local_subclass_publishes_parenless_supertype() {
    let source = r#"
open class Root(val value: String)
fun make(): Any {
    open class Base : Root {
        constructor(value: String) : super(value)
    }
    class Derived : Base {
        constructor() : super("OK")
    }
    return Derived()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalSecondarySupertype")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a parenless local superclass must finalize in Pass 1")
        .module
        .index();
    let derived = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter_map(|declaration| index.classifier_header(declaration))
        .find(|classifier| classifier.classifier.contains("Derived"))
        .expect("stable Derived classifier header");
    assert!(derived.superclass.is_some_and(|superclass| superclass
        .get()
        .obj_internal()
        .is_some_and(|owner| owner.contains("Base"))));
}

#[test]
fn explicit_body_local_signature_is_deferred_to_its_pass_two_context() {
    let source = r#"
class Environment(val value: Int, val block: Environment.() -> Unit)
fun box(): String {
    Environment(1, {
        class Local { val captured = value }
        Local().captured
    })
    return "OK"
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ExplicitBodyContextSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("explicit non-local headers must finalize without ordinary body semantics")
        .module
        .index();
    let captured = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("captured"))
        .expect("stable local property header");
    assert!(
        index.signature(captured).is_none(),
        "an explicit ordinary body must not enter the Pass-1 signature graph"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn assignment_receiver_anonymous_signature_sees_prior_lexical_value() {
    let source = r#"
interface Holder { var value: String }
fun <T> make(block: () -> T): T = block()
fun box(): String {
    val captured = "OK"
    make { object : Holder { override var value = captured } }.value = "changed"
    return "OK"
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("AssignmentReceiverSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "anonymous signatures inside assignment receivers must retain preceding lexical bindings"
    );
}

#[test]
fn nonlocal_generic_member_is_selected_instead_of_extracted_as_a_local_effect() {
    let inputs = [SourceInput::kotlin(
        "class Builder { fun append(value: String): Builder = this }\n\
         object Bug {\n\
             fun title(id: Int) = if (id == 0) \"OK\" else \"fail\"\n\
             private fun <T> T.header(id: Int) = Builder().append(title(id))\n\
             fun run() = header(0)\n\
         }\n",
    )
    .with_file_stem("NonlocalGenericMemberSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a stable nonlocal generic member call must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("run"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved run signature");
    assert!(result
        .result
        .get()
        .obj_internal()
        .is_some_and(|classifier| classifier.matches("Builder")));
}

#[test]
fn inferred_signature_demands_a_local_class_property_inside_a_receiver_lambda() {
    let inputs = [SourceInput::kotlin(
        "class Acc<E> { fun add(value: E): Boolean = true }\n\
         class Result<T>\n\
         fun <T> build(block: Acc<T>.() -> Unit): Result<T> = Result<T>()\n\
         class Owner {\n\
             val result = build {\n\
                 class Local { val action = { add(\"OK\") } }\n\
                 Local().action()\n\
             }\n\
         }\n",
    )
    .with_file_stem("LocalClassPropertySignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a demanded local property constraint must finalize only its enclosing signature")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved owner property signature")
        .result
        .get();
    assert!(result
        .obj_internal()
        .is_some_and(|classifier| classifier.matches("Result")));
    assert_eq!(result.type_args(), &[Ty::String]);
}

#[test]
fn unused_generic_local_declaration_does_not_block_unit_signature_inference() {
    let inputs = [SourceInput::kotlin(
        "fun <T> evaluate(block: () -> T): T = block()\n\
         val status = evaluate {\n\
             fun <F> bangbang(flag: F) = flag!!\n\
         }\n",
    )
    .with_file_stem("UnusedGenericLocalSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("an unused local declaration must not become signature-expression work")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("status"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved status signature");
    assert_eq!(result.result.get(), Ty::Unit);
}

#[test]
fn inner_secondary_constructor_defaults_are_available_to_signature_selection() {
    let inputs = [SourceInput::kotlin(
        "class Outer(val value: String) {\n\
             inner class Inner {\n\
                 constructor(number: Int = 1)\n\
             }\n\
         }\n\
         fun result() = Outer(\"OK\").Inner()\n",
    )
    .with_file_stem("InnerSecondaryConstructorDefaultSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("secondary-constructor defaults must participate in Pass 1 selection")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved result signature");
    assert!(result
        .result
        .get()
        .obj_internal()
        .is_some_and(|classifier| classifier.matches("Outer$Inner")));
}

#[test]
fn labeled_and_unlabeled_super_members_use_the_checked_receiver_scope() {
    let inputs = [SourceInput::kotlin(
        "open class Root { fun value() = \"OK\" }\n\
         class Host : Root() {\n\
             inner class Nested : Root() {\n\
                 fun immediate() = super.value()\n\
                 fun enclosing() = super@Host.value()\n\
             }\n\
         }\n",
    )
    .with_file_stem("ScopedSuperMemberSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("super member selection must finalize from the Pass 1 receiver scope")
        .module
        .index();
    for name in ["immediate", "enclosing"] {
        let signature = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("resolved {name} signature"));
        assert_eq!(signature.result.get(), Ty::String);
    }
}

#[test]
fn enum_entry_super_member_uses_the_enclosing_enum_as_its_direct_superclass() {
    let inputs = [SourceInput::kotlin(
        "enum class Entry {
             X { override fun value() = super.value() + \"#X\" };
             open fun value() = \"OK\"
         }
         fun result() = Entry.X.value()
",
    )
    .with_file_stem("EnumEntrySuperMemberSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("enum-entry super member signature must finalize in Pass 1")
        .module
        .index();
    let value_signatures = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("value"))
        .map(|declaration| {
            index
                .signature(declaration)
                .unwrap_or_else(|| panic!("resolved value signature for {declaration:?}"))
                .result
                .get()
        })
        .collect::<Vec<_>>();
    assert_eq!(value_signatures, vec![Ty::String, Ty::String]);
}

#[test]
fn bare_super_property_falls_through_to_a_direct_interface_default() {
    let inputs = [SourceInput::kotlin(
        "interface Base { val value: Int get() = 1 }
         open class Host : Base {
             override val value = super.value + 1
         }
",
    )
    .with_file_stem("InterfaceDefaultSuperPropertySignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("direct-interface super property signature must finalize in Pass 1")
        .module
        .index();
    let host_value = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("value"))
        .last()
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved Host.value signature");
    assert_eq!(host_value.result.get(), Ty::Int);
}

#[test]
fn same_named_context_property_does_not_replace_a_constructor_property_signature() {
    let inputs = [SourceInput::kotlin(
        "// LANGUAGE: +ContextParameters
         class Wrapper(val value: Int) {
             context(prefix: String)
             val value: String get() = prefix
         }
",
    )
    .with_file_stem("ContextPropertyOverloadSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("same-named property overloads must both finalize in Pass 1")
        .module
        .index();
    let results = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("value"))
        .filter_map(|declaration| index.signature(declaration))
        .map(|signature| signature.result.get())
        .collect::<Vec<_>>();
    assert_eq!(results, vec![Ty::Int, Ty::String]);
}

#[test]
fn named_companion_value_and_instance_member_keep_scope_tower_priority() {
    let inputs = [SourceInput::kotlin(
        "class Owner {\n\
             private val value: Int get() = 4\n\
             companion object Factory { val value: Int get() = 6 }\n\
             fun result() = value + Factory.value\n\
         }\n",
    )
    .with_file_stem("NamedCompanionValueSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the named companion must bind as a lexical singleton value")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved result signature");
    assert_eq!(result.result.get(), Ty::Int);
}

#[test]
fn java_static_field_qualifier_becomes_a_value_before_member_selection() {
    let source = r#"// WITH_STDLIB
        class Printer {
            fun print() = System.out.println("OK")
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("StaticFieldSignatureReceiver")];
    let stems = ["StaticFieldSignatureReceiver".to_string()];
    let mut paths = crate::toolchain::classpath_jars_for(source);
    paths.push(
        crate::toolchain::jdk_modules()
            .expect("Java static-field signature regression requires the configured JDK modules"),
    );
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a class-qualified Java static field must finalize as a value receiver")
        .module
        .index();
    let print = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("print"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved print signature");
    assert_eq!(print.result.get(), Ty::Unit);
}

#[test]
fn java_source_static_call_finalizes_through_classifier_candidates() {
    let inputs = [
        SourceInput::java("public class J { public static String value() { return \"OK\"; } }")
            .with_file_stem("J"),
        SourceInput::kotlin("fun box() = J.value()").with_file_stem("JavaStaticSignature"),
    ];
    let stems = ["J".to_string(), "JavaStaticSignature".to_string()];
    let mut paths = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    paths.push(
        crate::toolchain::jdk_modules()
            .expect("Java source static signature regression requires the configured JDK"),
    );
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a same-module Java static call must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved box signature");
    assert_eq!(result.result.get().non_null(), Ty::String);
}

#[test]
fn super_qualified_inherited_inner_constructor_uses_the_selected_receiver() {
    let inputs = [SourceInput::kotlin(
        "open class Base { inner class Item(val value: String) }\n\
         class Derived : Base() {\n\
             inner class Nested : Base() {\n\
                 fun immediate() = super.Item(\"nested\")\n\
                 fun enclosing() = super@Derived.Item(\"derived\")\n\
             }\n\
         }\n",
    )
    .with_file_stem("SuperInnerConstructorSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("super-qualified inner constructors must finalize in the selected receiver scope")
        .module
        .index();
    for name in ["immediate", "enclosing"] {
        let signature = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("resolved {name} signature"));
        assert!(signature
            .result
            .get()
            .obj_internal()
            .is_some_and(|classifier| classifier.matches("Base$Item")));
    }
}

#[test]
fn production_signature_selection_uses_complete_compact_call_shapes() {
    let source = r#"
fun <T> identity(value: T) = value
fun explicit() = identity<String>("explicit")
fun <T> first(vararg values: T) = values[0]
fun spread(values: Array<String>) = first<String>(*values)
fun reorder(first: String, second: Int) = first
fun named() = reorder(second = 1, first = "named")
fun defaulted(first: String = "default", second: Int) = first
fun namedDefault() = defaulted(second = 3)
class MemberNamed {
    fun reorder(first: String, second: Int) = first
    fun defaulted(first: String = "member", second: Int) = first
}
fun memberNamed(value: MemberNamed) = value.reorder(second = 2, first = "member")
fun memberNamedDefault(value: MemberNamed) = value.defaulted(second = 4)
class Mapper<T>(val value: T) {
    fun <R> map(transform: (T) -> R): R = transform(value)
    fun <R> mapDefault(ignored: Int = 0, transform: (T) -> R): R = transform(value)
}
fun implicitLambda(value: Mapper<String>) = value.map { it.length }
fun namedLambda(value: Mapper<String>) = value.map { item -> item.length }
fun constantLambda(value: Mapper<Int>) = value.map { "constant" }
fun memberNamedLambda(value: Mapper<String>) = value.mapDefault(transform = { it.length })
fun <T, R> apply(value: T, transform: (T) -> R): R = transform(value)
fun topLevelLambda() = apply("top") { it.length }
fun <T, R> applyDefault(value: T, ignored: Int = 0, transform: (T) -> R): R = transform(value)
fun topLevelNamedLambda() = applyDefault(transform = { it.length }, value = "named top")
fun <R> supply(block: () -> R): R = block()
fun supplied() = supply { "supplied" }
fun explicitInvoke() = { value: String -> value }.invoke("invoked")
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("CallShape")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("complete call shapes must finalize before body checking");
    let signature_result = |name: &str| {
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
                span,
                crate::fir::DeclarationKind::Function,
            ))
            .unwrap()
            .result
            .get()
    };
    for (name, expected) in [
        ("explicit", Ty::String),
        ("spread", Ty::String),
        ("named", Ty::String),
        ("namedDefault", Ty::String),
        ("memberNamed", Ty::String),
        ("memberNamedDefault", Ty::String),
        ("constantLambda", Ty::String),
        ("supplied", Ty::String),
        ("explicitInvoke", Ty::String),
        ("implicitLambda", Ty::Int),
        ("namedLambda", Ty::Int),
        ("topLevelLambda", Ty::Int),
        ("memberNamedLambda", Ty::Int),
        ("topLevelNamedLambda", Ty::Int),
    ] {
        assert_eq!(signature_result(name), expected, "{name}");
    }
}

#[test]
fn safe_extension_function_value_invocation_finalizes_before_body_streaming() {
    let source = r#"
fun invokeSafely(receiver: Int?, operation: Int.(Int) -> Int) = receiver?.operation(1)
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("SafeExtensionFunctionValue")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("safe extension-function-value result must finalize in Pass 1")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("invokeSafely"))
        .expect("stable function declaration");
    assert_eq!(
        index
            .signature(declaration)
            .expect("finalized function signature")
            .result
            .get(),
        Ty::nullable(Ty::Int),
    );
}

#[test]
fn typed_local_extension_function_value_is_callable_on_its_receiver_in_signature_inference() {
    assert_streaming_frontend(
        r#"
fun wrap(block: () -> Unit): Unit = block()
fun inferred() = wrap {
    val action: String.() -> Unit = {}
    "OK".action()
}
"#,
        "LocalExtensionFunctionValueSignature",
    );
}

#[test]
fn production_signature_selection_composes_callable_properties_with_implicit_context() {
    let source = r#"
// LANGUAGE: +ContextParameters
class Action {
    context(value: String) operator fun invoke() = value
}
class Owner {
    val action = Action()
    context(value: String) fun operatorResult() = action()
}
val function: context(String) () -> String = { "OK" }
context(value: String) fun functionResult() = function()
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ContextInvoke")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "callable property signatures must finalize before body checking"
    );
}

#[test]
fn production_signature_selection_tracks_receiver_lambda_member_extensions() {
    let source = r#"
// LANGUAGE: +ContextParameters
fun <T, R> scoped(receiver: T, block: T.() -> R): R = receiver.block()
class Node(val text: String) {
    context(context: Node)
    fun Node.render() = context.text + this@Node.text + text
}
fun box() = scoped(Node("D")) { render() + Node("E").render() }
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ReceiverMemberExtension")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("receiver-lambda member-extension signatures must finalize");
    let box_span = analysis.files[0]
        .decls
        .iter()
        .find_map(|declaration| match analysis.files[0].decl(*declaration) {
            crate::ast::Decl::Fun(function) if function.name == "box" => Some(function.span),
            crate::ast::Decl::Fun(_)
            | crate::ast::Decl::Class(_)
            | crate::ast::Decl::Property(_) => None,
        })
        .unwrap();
    let result = streamed
        .module
        .index()
        .signature(stable_declaration_at(
            &analysis,
            box_span,
            crate::fir::DeclarationKind::Function,
        ))
        .unwrap()
        .result
        .get();
    assert_eq!(result, Ty::String);
}

#[test]
fn implicit_context_arguments_shape_extension_calls_in_signature_graph() {
    let source = r#"
// LANGUAGE: +ContextParameters
fun <T, R> withReceiver(receiver: T, block: T.() -> R): R = receiver.block()
class Context(val text: String)
class Target
context(context: Context)
fun <R> Target.compute(block: context(Context) Target.() -> R) = block(context, this)
fun result() = withReceiver(Context("OK")) { Target().compute { text } }
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ContextExtensionSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("context extension calls must finalize before Pass 2")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .expect("stable result declaration");
    assert_eq!(
        index
            .signature(declaration)
            .expect("finalized result signature")
            .result
            .get(),
        Ty::String,
    );
}

#[test]
fn inferred_extension_signature_selects_argumented_member_extension_from_dispatch_receiver() {
    let source = r#"
interface Carrier<T> {
    fun <R> R.combine(value: T): String
}
class Use : Carrier<String> {
    override fun <R> R.combine(value: String) = value
}
fun Carrier<String>.result() = 0.combine("OK")
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ArgumentedMemberExtension")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("argumented member-extension signatures must finalize");
    let result_span = analysis.files[0]
        .decls
        .iter()
        .find_map(|declaration| match analysis.files[0].decl(*declaration) {
            crate::ast::Decl::Fun(function) if function.name == "result" => Some(function.span),
            crate::ast::Decl::Fun(_)
            | crate::ast::Decl::Class(_)
            | crate::ast::Decl::Property(_) => None,
        })
        .expect("result declaration");
    let result = streamed
        .module
        .index()
        .signature(stable_declaration_at(
            &analysis,
            result_span,
            crate::fir::DeclarationKind::Function,
        ))
        .expect("resolved result signature")
        .result
        .get();
    assert_eq!(result, Ty::String);
}

#[test]
fn inferred_property_constructor_call_does_not_require_default_abi_realization() {
    let inputs = [SourceInput::kotlin(
        r#"class Built(val text: String = "OK")
        val inferred = Built()"#,
    )
    .with_file_stem("DefaultConstructorSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "semantic constructor selection must finalize before backend default-call realization"
    );
}

#[test]
fn inferred_property_reference_finalizes_before_body_streaming() {
    let inputs = [SourceInput::kotlin(
        r#"val top = "O"
        val reference = ::top"#,
    )
    .with_file_stem("PropertyReferenceSignature")];
    let stems = ["PropertyReferenceSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "property-reference types must be demanded through the compact signature graph: {:?}",
        diagnostics.diags,
    );
}

#[test]
fn companion_lateinit_reference_finalizes_inferred_boolean_signature() {
    let inputs = [SourceInput::kotlin(
        r#"// LANGUAGE: +CompanionBlocksAndExtensions
        class C {
            companion {
                lateinit var value: String
                fun initialized() = ::value.isInitialized
            }
        }"#,
    )
    .with_file_stem("CompanionLateinitSignature")];
    let stems = ["CompanionLateinitSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::from_source(inputs[0].text),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("lateinit declaration selection must finish before Pass 2")
        .module
        .index();
    let initialized = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("initialized"))
        .and_then(|declaration| index.signature(declaration))
        .expect("finalized initialized signature");
    assert_eq!(initialized.result.get(), Ty::Boolean);
}

#[test]
fn inferred_signature_invokes_nominal_constructor_reference_through_callable_shape() {
    let inputs = [SourceInput::kotlin(
        r#"// WITH_STDLIB
        class Made
        fun result() = (::Made).let { it() }"#,
    )
    .with_file_stem("ConstructorReferenceInvokeSignature")];
    let stems = ["ConstructorReferenceInvokeSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("constructor-reference invocation must finalize before body streaming")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find_map(|declaration| {
            (index.declaration_name(declaration) == Some("result"))
                .then(|| index.signature(declaration))
                .flatten()
        })
        .expect("resolved result signature")
        .result
        .get();
    assert!(
        result
            .obj_internal()
            .is_some_and(|classifier| classifier.matches("Made")),
        "constructor-reference invocation must return its classifier: {result:?}"
    );
}

#[test]
fn inferred_signatures_select_nested_and_unbound_inner_constructor_references() {
    let inputs = [SourceInput::kotlin(
        r#"// WITH_STDLIB
        class Outer {
            class Nested(val result: String)
            inner class Inner(val result: String)
        }
        fun nestedResult() = (Outer::Nested).let { it("nested") }.result
        fun innerResult() = (Outer::Inner).let { it(Outer(), "inner") }.result"#,
    )
    .with_file_stem("NestedConstructorReferenceSignature")];
    let stems = ["NestedConstructorReferenceSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("nested constructor references must finalize before body streaming")
        .module
        .index();
    for name in ["nestedResult", "innerResult"] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find_map(|declaration| {
                (index.declaration_name(declaration) == Some(name))
                    .then(|| index.signature(declaration))
                    .flatten()
            })
            .expect("resolved constructor-reference result")
            .result
            .get();
        assert_eq!(result, Ty::String, "{name}");
    }
}

#[test]
fn singleton_qualified_nested_constructor_reference_finalizes_in_member_signature() {
    let source = r#"
fun interface Factory<T, R> { fun make(value: T): R }
object Outer {
    class Nested(val result: String)
}
class Holder {
    val factory = Factory(Outer::Nested)
}
fun box() = Holder().factory.make("OK").result
"#;
    let inputs =
        [SourceInput::kotlin(source).with_file_stem("SingletonNestedConstructorReference")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a singleton-qualified nested constructor reference must finalize in Pass 1")
        .module
        .index();
    let factory = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("factory"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved factory signature");
    assert_eq!(
        factory.result.get(),
        Ty::obj_args("Factory", &[Ty::String, Ty::obj("Outer$Nested")],)
    );
}

#[test]
fn inner_classifier_signatures_retain_captured_outer_type_arguments() {
    let inputs = [SourceInput::kotlin(
        r#"class Outer<O>(val outer: O) {
            inner class Inner<I>(val inner: I) {
                val captured: O get() = outer
                fun <R> replace(value: R): Inner<R> = Inner(value)
            }
            fun <I> create(value: I) = Inner(value)
        }
        fun resolvedCaptured(value: Outer<Int>) = value.create("inner").replace(1L).captured"#,
    )
    .with_file_stem("InnerCapturedSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("captured classifier applications must finalize before body streaming")
        .module
        .index();
    for name in ["create", "replace"] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .expect("inner-class callable signature")
            .result
            .get();
        assert_eq!(
            result.type_args().len(),
            2,
            "{name} must carry its own and captured classifier arguments: {result:?}"
        );
    }
    let captured = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("resolvedCaptured"))
        .and_then(|declaration| index.signature(declaration))
        .expect("captured result signature");
    assert_eq!(captured.result.get(), Ty::Int);
}

#[test]
fn qualified_companion_singleton_reference_is_bound_during_signature_solving() {
    let inputs = [SourceInput::kotlin(
        r#"// WITH_STDLIB
        class Owner {
            companion object { fun value() = "OK" }
        }
        fun result() = (Owner.Companion::value).let { it() }"#,
    )
    .with_file_stem("BoundCompanionReferenceSignature")];
    let stems = ["BoundCompanionReferenceSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "qualified companion references must finalize as bound singleton references"
    );
}

#[test]
fn object_property_reference_is_bound_in_the_signature_graph() {
    let inputs = [SourceInput::kotlin(
        r#"object Owner { val value = "OK" }
        val reference = Owner::value"#,
    )
    .with_file_stem("ObjectPropertyReferenceSignature")];
    let stems = ["ObjectPropertyReferenceSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("object property reference must finalize before body streaming");
    let reference_type = (0..streamed.module.index().declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find_map(|declaration| {
            (streamed.module.index().declaration_name(declaration) == Some("reference"))
                .then(|| streamed.module.index().signature(declaration))
                .flatten()
        })
        .unwrap()
        .result
        .get();
    assert!(
        reference_type
            .obj_internal()
            .is_some_and(|name| name.matches("kotlin/reflect/KProperty0")),
        "an object receiver is a bound value, not an unbound classifier: {reference_type:?}"
    );
}

#[test]
fn natural_bound_callable_reference_prefers_the_member_rung_over_extensions() {
    let inputs = [SourceInput::kotlin(
        r#"// WITH_STDLIB
        val unbound = String::plus
        val bound = ""::plus"#,
    )
    .with_file_stem("NaturalMemberCallableReferenceSignature")];
    let stems = ["NaturalMemberCallableReferenceSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "the member callable-reference rung must finalize before extensions are considered"
    );
}

#[test]
fn compact_inner_class_literal_signature_applies_explicit_outer_arguments() {
    let source = r#"// WITH_STDLIB
// LANGUAGE: +ProperSupportOfInnerClassesInCallableReferenceLHS
class ClassReference<A> {
    inner class A<K> {
        inner class DeepInner
        val refFoo = A<K>::DeepInner::class
    }
}
val refBar = ClassReference<Int>.A<String>::DeepInner::class
val raw = ClassReference.A.DeepInner::class
open class Base<X : String>(val value: X) {
    inner class B<Y> { fun bar(): X = value }
}
class Child : Base<String>("") {
    val refMember = B<Int>::bar
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("InnerClassLiteralSignature")];
    let stems = ["InnerClassLiteralSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("inner class-literal signature must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("refBar"))
        .and_then(|declaration| index.signature(declaration))
        .expect("finalized refBar property signature")
        .result
        .get();
    let represented = result
        .type_args()
        .first()
        .copied()
        .expect("KClass represented type");
    assert_eq!(represented.type_args(), &[Ty::String, Ty::Int]);

    let member_reference = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("refMember"))
        .and_then(|declaration| index.signature(declaration))
        .expect("finalized inherited inner member-reference signature")
        .result
        .get();
    assert_eq!(member_reference.type_args().last(), Some(&Ty::String));
    assert!(
        !member_reference.mentions_ty_param(),
        "the applied Base<String> receiver must specialize its inherited inner member: {member_reference:?}"
    );
}

#[test]
fn delegated_signature_checks_bound_property_reference_against_reflective_parameter() {
    let source = r#"// WITH_STDLIB
        import kotlin.reflect.KProperty
        import kotlin.reflect.KProperty0

        class Delegate(val reference: KProperty0<UInt>) {
            operator fun getValue(thisRef: Any?, property: KProperty<*>): UByte =
                reference().toUByte()
        }

        class Owner {
            val value = 1u
            val delegated by Delegate(this::value)
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("BoundPropertyDelegateSignature")];
    let stems = ["BoundPropertyDelegateSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the bound property reference must satisfy its reflective constructor parameter")
        .module
        .index();
    let delegated = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("delegated"))
        .and_then(|declaration| index.signature(declaration))
        .expect("resolved delegated-property signature");
    assert_eq!(delegated.result.get(), Ty::UByte);
}

#[test]
fn nested_object_value_resolves_in_a_delegated_signature() {
    let inputs = [SourceInput::kotlin(
        r#"object Outer { object Delegate }
        operator fun Outer.Delegate.getValue(thisRef: Any?, property: Any?): String = "OK"
        val result by Outer.Delegate"#,
    )
    .with_file_stem("NestedObjectDelegateSignature")];
    let stems = ["NestedObjectDelegateSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "nested object identity must resolve while the compact signature graph is active"
    );
}

#[test]
fn extension_delegate_conventions_receive_the_property_extension_receiver() {
    let inputs = [SourceInput::kotlin(
        r#"object Host {
            class StringDelegate(val suffix: String) {
                operator fun getValue(receiver: String, property: Any): String = receiver + suffix
            }
            operator fun String.provideDelegate(host: Any?, property: Any): StringDelegate =
                StringDelegate(this)
            val String.plusK by "K"
            val result = "O".plusK
        }"#,
    )
    .with_file_stem("ExtensionDelegateSignature")];
    let stems = ["ExtensionDelegateSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("extension delegate signatures must finalize before body streaming")
        .module
        .index();
    for name in ["plusK", "result"] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find_map(|declaration| {
                (index.declaration_name(declaration) == Some(name))
                    .then(|| index.signature(declaration))
                    .flatten()
            })
            .unwrap_or_else(|| panic!("missing finalized signature for {name}"));
        assert_eq!(result.result.get(), Ty::String, "signature for {name}");
    }
}

#[test]
fn same_named_delegated_extensions_form_distinct_signature_dependencies() {
    let inputs = [SourceInput::kotlin(
        r#"open class Base
        object Left : Base()
        object Right : Base()
        class Delegate(val value: String) {
            operator fun getValue(receiver: Base, property: Any): String = value
        }
        val Left.label by Delegate("L")
        val Right.label by Delegate("R")
        fun result() = Left.label + Right.label"#,
    )
    .with_file_stem("OverloadedExtensionDelegateSignature")];
    let stems = ["OverloadedExtensionDelegateSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("same-named delegated extensions must finalize independently")
        .module
        .index();
    let labels = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("label"))
        .map(|declaration| index.signature(declaration).unwrap().result.get())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec![Ty::String, Ty::String]);
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .unwrap();
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn inline_delegate_preparation_does_not_commit_unchecked_constructor_types() {
    let inputs = [SourceInput::kotlin(
        r#"class Delegate(val value: String)
        interface Owner { val suffix: String }
        inline operator fun Delegate.getValue(owner: Owner, property: Any): String =
            value + owner.suffix
        class Concrete(override val suffix: String) : Owner
        val Concrete.label by Delegate("O")
        fun result() = Concrete("K").label"#,
    )
    .with_file_stem("InlineDelegatePreparation")];
    let stems = ["InlineDelegatePreparation".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("inline preparation must leave unrelated constructor headers to the full check")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .unwrap();
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn explicit_generic_constructor_shapes_a_trailing_lambda_signature() {
    let inputs = [SourceInput::kotlin(
        r#"class Item { fun value(): Int = 1 }
        class Holder<T>(val project: (T) -> Int)
        val inferred = Holder<Item> { item -> item.value() }"#,
    )
    .with_file_stem("GenericConstructorLambdaSignature")];
    let stems = ["GenericConstructorLambdaSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert!(
        analysis.streamed.is_some(),
        "constructor selection must shape the lambda through its explicit classifier argument"
    );
}

#[test]
fn expected_callable_reference_specializes_a_postponed_generic_argument() {
    let inputs = [SourceInput::kotlin(
        r#"fun <T> id(value: T): T = value
        fun <T> intersect(left: T, right: T): T = left
        fun <T, R> T.let(block: (T) -> R): R = block(this)
        class C1 { override fun toString() = "C1" }
        class C2
        fun box() = intersect(C1(), C2()).let(::id).toString()"#,
    )
    .with_file_stem("ExpectedCallableReferenceSignature")];
    let stems = ["ExpectedCallableReferenceSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the expected function shape must resolve the generic callable reference")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("box signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn expected_callable_reference_demands_an_inferred_source_signature() {
    let source = r#"fun inferredTarget() = "OK"
        val apply: (() -> String) -> String = { block -> block() }
        val result = apply(::inferredTarget)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("InferredReferenceTargetSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the referenced source signature must finalize on demand")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("result signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn adapted_reference_to_an_imported_object_member_demands_its_inferred_signature() {
    let source = r#"import Host.foo
        fun withO(fn: (String) -> String) = fn("O")
        object Host { fun foo(vararg values: String) = values[0] + "K" }
        fun box() = withO(::foo)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ImportedObjectReferenceSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the imported singleton member signature must finalize on demand")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("box signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn callable_valued_generic_extension_property_is_specialized_before_invoke() {
    let source = r#"val <T : Any> T.callable
            get() = { ignored: T -> this }
        fun result(ok: String, ignored: String) = ok.callable(ignored)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ExtensionPropertyInvokeSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the inferred extension-property function type must finalize before invocation")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("result signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn companion_member_supplies_lambda_expectation_to_an_enclosing_class_body() {
    let source = r#"class Holder {
            fun result() = combine { "K" }
            companion object {
                private inline fun combine(block: () -> String) = "O" + block()
            }
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("CompanionLambdaExpectation")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the enclosing companion rung must shape and resolve the lambda call")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("result signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn postponed_receiver_lambda_constraints_finalize_an_inferred_property() {
    let inputs = [SourceInput::kotlin(
        r#"class Buildee<T : Any> { fun consume(value: T) {} }
        fun <T : Any> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()
        class Klass { val buildee = build { consume("OK") } }"#,
    )
    .with_file_stem("PostponedReceiverSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("postponed receiver constraints must finalize in Pass 1")
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
fn pcla_constraints_and_conditional_sibling_finalize_an_inferred_function() {
    let source = r#"interface Consumer<in T>
        fun <T> buildConsumer(block: (Consumer<T>) -> Unit): T? = null
        fun <T> materialize(): T = "K" as T
        fun expectConsumerString(value: Consumer<String>) {}
        fun inferred() = buildConsumer { expectConsumerString(it) } ?: materialize()
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("PclaConditionalSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("PCLA conditional signature must finalize in Pass 1")
        .module
        .index();
    let inferred = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("inferred"))
        .and_then(|declaration| index.signature(declaration))
        .expect("inferred signature");
    assert_eq!(inferred.result.get(), Ty::String);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn pcla_receiver_constraints_cross_nested_local_classifier_scopes() {
    let source = r#"class Builder<T> { fun add(value: T) {} }
        class Result<T>
        fun <T> build(block: Builder<T>.() -> Unit): Result<T> = Result<T>()
        class Host {
            val inferred = build {
                class Local {
                    val action = { add("OK") }
                }
                Local().action()
            }
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedPclaScopeSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("nested PCLA scope must finalize in Pass 1")
        .module
        .index();
    let inferred = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("inferred"))
        .and_then(|declaration| index.signature(declaration))
        .expect("inferred signature");
    assert_eq!(inferred.result.get().type_args(), &[Ty::String]);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

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
        Box::new(EmptySymbolSource),
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
fn explicit_generic_call_shapes_bound_extension_reference_for_sam_signature() {
    let inputs = [SourceInput::kotlin(
        r#"fun interface SamInterface {
            fun (Int.() -> String).accept(): String
        }
        fun (Int.() -> String).foo(argument: Int.() -> String): String = "OK"
        fun <T> test(): Int.() -> T = { "" as T }
        val bound = SamInterface(test<String>()::foo)"#,
    )
    .with_file_stem("GenericBoundExtensionSamSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the explicit generic result must shape the bound extension reference")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("bound"))
        .and_then(|declaration| index.signature(declaration))
        .expect("SAM property signature")
        .result
        .get();
    assert!(
        result
            .obj_internal()
            .is_some_and(|classifier| classifier.matches("SamInterface")),
        "bound extension reference must produce the selected SAM type: {result:?}"
    );
}

#[test]
fn inferred_extension_bound_reference_selects_declared_member_without_a_false_cycle() {
    let source = "abstract class Foo { abstract fun contains(value: Int) }\n\
                  fun consume(values: IntArray, action: (Int) -> Unit): Unit {}\n\
                  fun Foo.contains(vararg values: Int) = consume(values, this::contains)\n";
    let inputs = [SourceInput::kotlin(source).with_file_stem("BoundCallableReferenceMember")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the declared member must break the apparent extension cycle")
        .module
        .index();
    let contains_results = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("contains"))
        .filter_map(|declaration| index.signature(declaration))
        .map(|signature| signature.result.get())
        .collect::<Vec<_>>();
    assert_eq!(contains_results, [Ty::Unit, Ty::Unit]);
}

#[test]
fn compact_graph_finalizes_an_extension_property_after_legacy_approximation_declines() {
    let source = r#"// WITH_STDLIB
        interface ColumnType<V>
        interface KeyColumnType<V> : ColumnType<V>
        open class Column<V, T : ColumnType<V>>
        typealias KeyColumn<V> = Column<V, out KeyColumnType<V>>

        sealed class Key
        data class PartitionKey<P>(val partitionKey: KeyColumn<P>) : Key()
        data class CompositeKey<P, S>(
            val partitionKey: KeyColumn<P>,
            val sortKey: KeyColumn<S>,
        ) : Key()

        val Key.columns
            get() = when (this) {
                is PartitionKey<*> -> listOf(partitionKey)
                is CompositeKey<*, *> -> listOf(partitionKey, sortKey)
            }
        val Key.columnsSet
            get() = columns.toMutableSet()
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("CapturedExtensionPropertySignature")];
    let stems = ["CapturedExtensionPropertySignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the compact graph must own the inferred extension result after legacy decline")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("columnsSet"))
        .and_then(|declaration| index.signature(declaration))
        .expect("finalized columnsSet signature")
        .result
        .get();
    assert!(
        result
            .obj_internal()
            .is_some_and(|classifier| classifier.matches("kotlin/collections/MutableSet")),
        "the compact graph must publish the semantic mutable-set result: {result:?}"
    );
}

#[test]
fn demanded_anonymous_property_keeps_its_enclosing_receiver_lambda_scope() {
    let source = r#"fun <T, R> withReceiver(receiver: T, block: T.() -> R): R = receiver.block()
        object Provider {
            operator fun Any?.get(key: String): String = "OK"
        }
        object Owner {
            fun result() = withReceiver(Provider) {
                val value = object { val nested = 1["key"] }
                value.nested
            }
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("AnonymousReceiverScopeSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the enclosing receiver-lambda rung must survive demanded local signature solving")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("finalized result signature")
        .result
        .get();
    assert_eq!(result, Ty::String);
}

#[test]
fn contextual_extension_lambda_adopts_its_declared_result_in_a_generic_member() {
    let source = r#"fun <T> T.runExt(fn: T.() -> String) = fn()
        class Receiver<T : String>(private val value: T) {
            fun test() = runExt { value }
        }
        fun box() = Receiver("OK").test()
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("GenericReceiverLambdaSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("contextual extension lambda must finalize in Pass 1")
        .module
        .index();
    for name in ["test", "box"] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("missing finalized signature for {name}"));
        assert_eq!(result.result.get(), Ty::String);
    }
}

#[test]
fn inherited_generic_member_extension_is_visible_inside_a_receiver_lambda() {
    let source = r#"// WITH_STDLIB
        class NoiseMaker { fun say(value: String) {} }
        fun noiseMaker(block: NoiseMaker.() -> Unit) {}
        abstract class Pet {
            fun <T> NoiseMaker.playWith(friend: T) { say(friend.toString()) }
        }
        class Doggy : Pet() {
            fun play() = noiseMaker {
                say("hello")
                playWith("friend")
            }
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("InheritedMemberExtensionSignature")];
    let stems = ["InheritedMemberExtensionSignature".to_string()];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("inherited member-extension selection must finalize in Pass 1")
        .module
        .index();
    let play = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("play"))
        .and_then(|declaration| index.signature(declaration))
        .expect("play signature");
    assert_eq!(play.result.get(), Ty::Unit);
}

#[test]
fn explicit_type_arguments_specialize_a_projected_sam_lambda_in_pass_one() {
    let source = r#"fun interface Comparator<in T> {
            fun compare(first: T, second: T): Int
        }
        fun <T> compareWith(comparator: Comparator<in T>, first: T, second: T) =
            comparator.compare(first, second)
        fun ordered(first: Int, second: Int) =
            compareWith<Int>({ left, right -> left - right }, first, second)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ProjectedSamSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("projected SAM lambda must finalize in Pass 1")
        .module
        .index();
    let ordered = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("ordered"))
        .and_then(|declaration| index.signature(declaration))
        .expect("ordered signature");
    assert_eq!(ordered.result.get(), Ty::Int);
}

#[test]
fn typed_sam_lambda_keeps_implicit_extension_and_context_inputs() {
    let source = r#"
// LANGUAGE: +ContextParameters
fun interface ExtensionSam {
    fun Int.accept(value: String): String
}
val extensionObject = ExtensionSam { value: String -> value }

class Context {
    fun accept(value: String): String = value
}
context(context: T)
fun <T> implicit(): T = context
fun interface ContextSam {
    context(context: Context)
    fun accept(value: String): String
}
val contextObject = ContextSam { value: String -> implicit<Context>().accept(value) }
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("TypedSamLambdaInputs")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("typed SAM lambdas must finalize with their implicit inputs")
        .module
        .index();
    for (property, classifier) in [
        ("extensionObject", "ExtensionSam"),
        ("contextObject", "ContextSam"),
    ] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(property))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("finalized {property} signature"))
            .result
            .get();
        assert!(
            result
                .obj_internal()
                .is_some_and(|internal| internal.matches(classifier)),
            "{property} resolved to {result:?}",
        );
    }
}

#[test]
fn unqualified_generic_sam_constructor_publishes_applied_result() {
    let source = r#"fun interface Target<T, R> {
            fun apply(value: T): R
        }
        fun <FT, FR> adapt(lambda: (FT) -> FR) = Target(lambda)
        fun box(): String = adapt<String, String> { "O" + it }.apply("K")
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("GenericSamConstructorSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the generic SAM constructor must finalize in Pass 1")
        .module
        .index();
    let adapt = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("adapt"))
        .and_then(|declaration| index.signature(declaration))
        .expect("adapt signature");
    let result = adapt.result.get();
    assert!(
        result
            .obj_internal()
            .is_some_and(|classifier| classifier.matches("Target")),
        "the SAM constructor must retain its selected classifier: {result:?}",
    );
    let parameter = adapt.parameters[0].get();
    let function_parameters = parameter.fun_params().expect("function parameter shape");
    let function_result = parameter.fun_ret().expect("function result shape");
    assert_eq!(
        result.type_args(),
        [function_parameters[0], function_result],
        "the SAM result must retain both caller-owned type variables",
    );
}

#[test]
fn qualified_nested_fun_interface_constructor_finalizes_in_pass_one() {
    let source = r#"interface Outer {
            fun interface Nested : Outer {
                operator fun invoke()
            }
        }
        val instance = Outer.Nested { }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("QualifiedNestedSamSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("qualified nested SAM construction must finalize in Pass 1")
        .module
        .index();
    let instance = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("instance"))
        .and_then(|declaration| index.signature(declaration))
        .expect("instance signature");
    assert_eq!(instance.result.get(), Ty::obj("Outer$Nested"));
}

#[test]
fn package_qualified_internal_sibling_property_finalizes_in_pass_one() {
    let inputs = [
        SourceInput::kotlin("fun box() = sample.value\n").with_file_stem("Use"),
        SourceInput::kotlin("package sample\ninternal val value: String = \"OK\"\n")
            .with_file_stem("Value"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("package-qualified sibling property must finalize in Pass 1")
        .module
        .index();
    let function = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("box signature");
    assert_eq!(function.result.get(), Ty::String);
}

#[test]
fn type_alias_constructor_calls_and_references_preserve_fixed_arguments() {
    let source = r#"fun interface Target<A, B> { fun invoke(value: A): B }
        typealias TargetOf<K> = Target<Int, K>
        class Holder<A, B>(val value: B)
        typealias HolderOf<K> = Holder<Int, K>
        fun <T, R> T.map(transform: (T) -> R): R = transform(this)

        fun directSam() = TargetOf { "direct" }
        fun directClass() = HolderOf("direct")
        fun referencedSam(value: (Int) -> String) = value.map(::TargetOf)
        fun referencedClass(value: String) = value.map(::HolderOf)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("TypeAliasConstructorSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("type-alias constructors must finalize in Pass 1")
        .module
        .index();
    for name in [
        "directSam",
        "directClass",
        "referencedSam",
        "referencedClass",
    ] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("missing finalized signature for {name}"))
            .result
            .get();
        assert_eq!(result.type_args(), &[Ty::Int, Ty::String], "{name}");
    }
}

#[test]
fn invoke_constraints_use_function_member_and_extension_operator_scopes() {
    let source = r#"class MemberCallable {
            operator fun invoke(value: Int) = value
        }
        class ExtensionCallable
        operator fun ExtensionCallable.invoke(value: String) = value

        fun functionValue(callable: (Int) -> Int, value: Int) = callable(value)
        fun memberValue(callable: MemberCallable, value: Int) = callable(value)
        fun extensionValue(callable: ExtensionCallable, value: String) = callable(value)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("InvokeConstraintScopes")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("invoke conventions must finalize in Pass 1")
        .module
        .index();
    for (name, expected) in [
        ("functionValue", Ty::Int),
        ("memberValue", Ty::Int),
        ("extensionValue", Ty::String),
    ] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("missing finalized signature for {name}"));
        assert_eq!(result.result.get(), expected, "{name}");
    }
}

#[test]
fn nested_class_lambda_reads_its_enclosing_private_companion_during_signature_solving() {
    let source = r#"fun <T> eval(fn: () -> T) = fn()
        class Outer {
            private companion object { val result = "OK" }
            class Nested { fun foo() = eval { result } }
            fun test() = Nested().foo()
        }
        fun box() = Outer().test()
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedCompanionLambdaSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("enclosing companion lookup must finalize in Pass 1")
        .module
        .index();
    for name in ["foo", "test", "box"] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("missing finalized signature for {name}"));
        assert_eq!(result.result.get(), Ty::String);
    }
}

#[test]
fn exposed_anonymous_object_signature_uses_its_declared_base_class() {
    let source = r#"open class A(open val value: String)
        open class B(open val value: String) {
            fun changed(next: String) = object : A("fail") {
                override val value = this@B.value + next
            }
        }
        fun box() = B("O").changed("K").value
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("AnonymousBaseSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("exposed anonymous results must finalize as their declared base class")
        .module
        .index();
    let changed = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("changed"))
        .and_then(|declaration| index.signature(declaration))
        .expect("changed signature");
    assert!(changed
        .result
        .get()
        .obj_internal()
        .is_some_and(|classifier| classifier.matches("A")));
    let boxed = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("box"))
        .and_then(|declaration| index.signature(declaration))
        .expect("box signature");
    assert_eq!(boxed.result.get(), Ty::String);
}

#[test]
fn public_signature_keeps_anonymous_local_exact_until_its_member_is_consumed() {
    let source = r#"fun <T, R> scoped(receiver: T, block: T.() -> R): R = receiver.block()
        object Receiver
        object Host {
            fun result() = scoped(Receiver) {
                val local = object { val value = "OK" }
                local.value
            }
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("AnonymousLocalSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the public signature must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("result"))
        .and_then(|declaration| index.signature(declaration))
        .expect("result signature");
    assert_eq!(result.result.get(), Ty::String);
}

#[test]
fn contextual_unit_lambda_coerces_its_value_result_during_signature_solving() {
    let source = r#"class Log { fun append(message: String): Log = this }
        val log = Log()
        fun <T> T.alsoLike(block: (T) -> Unit): T {
            block(this)
            return this
        }
        fun logged(message: String, value: Int) =
            value.alsoLike { log.append(message) }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ContextualUnitSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("Unit-coerced contextual lambda must finalize in Pass 1")
        .module
        .index();
    let logged = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("logged"))
        .and_then(|declaration| index.signature(declaration))
        .expect("logged signature");
    assert_eq!(logged.result.get(), Ty::Int);
}

#[test]
fn lambda_passed_to_any_uses_its_natural_function_type() {
    let source = r#"fun consume(block: Any): Int = 1
        fun inferred() =
            consume {
                try {}
                finally {
                    {}
                }
            }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NaturalLambdaSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("a lambda accepted as Any must finalize from its natural function type")
        .module
        .index();
    let inferred = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("inferred"))
        .and_then(|declaration| index.signature(declaration))
        .expect("inferred signature");
    assert_eq!(inferred.result.get(), Ty::Int);
}

#[test]
fn anonymous_function_keeps_its_declared_result_in_generic_signature_inference() {
    let source = r#"class Box(val value: String)
        fun use(box: Box): String = box.value
        fun <T> call(block: () -> T): T = block()
        fun inferred() = use(call(fun(): Box { return Box("OK") }))
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("AnonymousFunctionSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("the anonymous function's declared result must finalize in Pass 1")
        .module
        .index();
    let inferred = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("inferred"))
        .and_then(|declaration| index.signature(declaration))
        .expect("inferred signature");
    assert_eq!(inferred.result.get(), Ty::String);
}

#[test]
fn generic_constructor_adapts_integer_literal_through_ordinary_class_bound() {
    let source = r#"class Holder<T : Long>(val value: T)
        fun consume(holder: Holder<Long>): String = "OK"
        fun inferred() = consume(Holder(0))
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("BoundedConstructorSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("bounded ordinary constructor inference must finalize in Pass 1")
        .module
        .index();
    let inferred = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("inferred"))
        .and_then(|declaration| index.signature(declaration))
        .expect("inferred signature");
    assert_eq!(inferred.result.get(), Ty::String);
}

#[test]
fn boolean_and_and_if_conditions_refine_signature_operands() {
    let source = r#"fun nullable(a: Double?, b: Double?) =
            a != null && b != null && a < b
        fun checked(a: Any?, b: Any?) =
            if (a is Double && b is Double) a < b else null!!
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("SmartCastSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("condition smart casts must finalize in Pass 1")
        .module
        .index();
    for name in ["nullable", "checked"] {
        let result = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("missing finalized signature for {name}"));
        assert_eq!(result.result.get(), Ty::Boolean);
    }
}

#[test]
fn callable_references_in_a_generic_vararg_use_the_selected_element_expectation() {
    let source = r#"fun zero() {}
        fun one(value: Any) {}
        fun <T> collect(vararg values: T): Array<T> = values
        val references = collect<Any>(::zero, ::one)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("CallableReferenceVarargSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("generic vararg callable references must finalize in Pass 1")
        .module
        .index();
    let signature = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("references"))
        .and_then(|declaration| index.signature(declaration))
        .expect("references signature");
    let result = signature.result.get();
    assert!(result
        .obj_internal()
        .is_some_and(|classifier| classifier.matches("kotlin/Array")));
    assert_eq!(result.type_args(), &[Ty::obj("kotlin/Any")]);
}

#[test]
fn nested_builder_constraints_finalize_the_enclosing_signature() {
    let source = r#"fun <A, B> build(block: Builder<A>.() -> B): Provider<A, B> =
            object : Provider<A, B> { override fun value(): A = "OK" as A }
        fun <A, B> build2(block: Builder<A>.() -> B): Provider<A, B> =
            object : Provider<A, B> { override fun value(): A = "OK" as A }
        interface Builder<T> {
            fun <R> get(provider: Provider<T, R>): R
            fun <R> get2(provider: Provider<T, R>): R
        }
        interface Provider<T, R> { fun value(): T }
        val fixed: Provider<Any, Any> =
            object : Provider<Any, Any> { override fun value(): Any = "fixed" }
        val inferred = build {
            get(build2 {
                get2(fixed)
            })
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedBuilderSignature")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("nested builder constraints must finalize in Pass 1")
        .module
        .index();
    let inferred = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("inferred"))
        .and_then(|declaration| index.signature(declaration))
        .expect("inferred signature");
    assert!(inferred
        .result
        .get()
        .obj_internal()
        .is_some_and(|classifier| classifier.matches("Provider")));
    assert_eq!(
        inferred.result.get().type_args(),
        &[Ty::obj("kotlin/Any"), Ty::obj("kotlin/Any")]
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn star_captured_self_bound_keeps_the_callers_type_parameter_in_checked_fir() {
    let source = r#"interface Entity<D, S : Entity<D, S>> {
            fun <T : S> isEqualTo(expected: Any?): T
        }
        fun <U : Any> compare(entity: Entity<U, *>, expected: U) {
            entity.isEqualTo(expected)
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("CapturedSelfBound")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn postponed_generic_result_preserves_its_non_null_formal_bound() {
    let source = r#"class Owner<T> {
            fun <X> constrain(source: Generic<X, T>): Generic<X, T> = source
        }
        class Generic<A, B>
        class Concrete
        fun <T> build(block: Owner<T>.() -> Generic<*, T>) {}
        fun <Y : Any> produce(): Generic<Y, Concrete> = Generic()
        fun box() {
            build {
                constrain(produce())
            }
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("PostponedBound")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn dependency_extension_constraints_finalize_the_enclosing_generic_signature() {
    let source = r#"// WITH_STDLIB
        import kotlin.coroutines.Continuation
        import kotlin.coroutines.resume
        class Host {
            fun <R> dialog(block: (Continuation<R>) -> Unit): R = null as R
            fun <R> await(initial: R) = dialog { continuation ->
                continuation.resume(initial)
            }
        }
        fun box(): String = Host().await("OK")
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DependencyExtensionConstraint")];
    let mut classpath = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        )),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("dependency extension constraints must finalize in Pass 1")
        .module
        .index();
    let signature = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("await"))
        .and_then(|declaration| index.signature(declaration))
        .expect("await signature");
    assert_eq!(signature.result, signature.parameters[0]);
}

#[test]
fn bounded_contract_declaration_publishes_by_stable_identity() {
    let source = r#"// WITH_STDLIB
        import kotlin.contracts.ExperimentalContracts
        import kotlin.contracts.contract

        sealed class Status {
            class Error(val text: String) : Status()
        }

        @OptIn(ExperimentalContracts::class)
        fun Status.isError(): Boolean {
            contract { returns(true) implies (this@isError is Status.Error) }
            return this is Status.Error
        }

        fun read(status: Status): String {
            if (status.isError()) return status.text
            return "OK"
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("BoundedSourceContract")];
    let mut classpath = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        )),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("source contracts must finalize in Pass 1")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("isError"))
        .expect("stable isError declaration");
    let contract = index
        .contract(declaration)
        .expect("Pass 1 must publish the source contract")
        .as_contract();
    let [crate::contracts::Effect::ConditionalReturns {
        returns: crate::contracts::ReturnsValue::Bool(true),
        conclusion:
            crate::contracts::Condition::IsType {
                param: crate::contracts::ParamRef::Receiver,
                ty: crate::contracts::ConditionType::Metadata(ty),
                negated: false,
            },
    }] = contract.effects.as_slice()
    else {
        panic!("unexpected resolved contract: {contract:?}");
    };
    assert_eq!(*ty, Ty::obj("Status$Error"));
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_contract_is_visible_when_caller_source_streams_first() {
    let caller = r#"
        fun read(status: Status): String {
            if (status.isError()) return status.text
            return "OK"
        }
    "#;
    let declaration = r#"// WITH_STDLIB
        import kotlin.contracts.ExperimentalContracts
        import kotlin.contracts.contract

        sealed class Status {
            class Error(val text: String) : Status()
        }

        @OptIn(ExperimentalContracts::class)
        fun Status.isError(): Boolean {
            contract { returns(true) implies (this@isError is Status.Error) }
            return this is Status.Error
        }
    "#;
    let inputs = [
        SourceInput::kotlin(caller).with_file_stem("Caller"),
        SourceInput::kotlin(declaration).with_file_stem("Declaration"),
    ];
    let mut classpath = crate::toolchain::classpath_jars_for(declaration);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        )),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(census.bodies, 4);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn constructor_parameter_rung_is_limited_to_class_initialization_signatures() {
    let source = r#"
        class Values(vararg xs: Int) { val xs = xs }
        open class Base { fun text(): String = "OK" }
        class BoundBox<T : Base>(private val value: T) {
            fun result() = value.text()
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ConstructorParameterScope")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("constructor-parameter scopes must finalize in Pass 1")
        .module
        .index();
    let signature = |name| {
        (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("stable signature for {name}"))
    };
    assert_eq!(signature("xs").result.get(), Ty::obj("kotlin/IntArray"));
    assert_eq!(signature("result").result.get(), Ty::String);
}

#[test]
fn alias_star_projection_publishes_target_bound_and_reads_without_projection_wrapper() {
    let source = r#"
        open class Node(val text: String)
        class Leaf : Node("OK")
        class Box<out T : Node>(val node: T)
        typealias Alias<T> = Box<T>
        fun expose(value: Box<Leaf>): Alias<*> = value
        fun read(value: Box<Leaf>) = expose(value).node.text
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("AliasStarBound")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("bounded alias star signatures must finalize in Pass 1")
        .module
        .index();
    let signature = |name| {
        (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| index.declaration_name(*declaration) == Some(name))
            .and_then(|declaration| index.signature(declaration))
            .unwrap_or_else(|| panic!("stable signature for {name}"))
    };
    assert_eq!(
        signature("expose").result.get().type_args(),
        &[Ty::star_projection(Ty::obj("Node"))]
    );
    assert_eq!(signature("read").result.get(), Ty::String);
}

#[test]
fn bounded_classifier_star_publishes_its_declared_upper_bound() {
    let source = r#"
        class C<out S : Any, out T : Any>
        class D(val value: C<*, *>)
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("BoundedClassifierStar")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("bounded star constructor signature must finalize in Pass 1")
        .module
        .index();
    let d = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|classifier| classifier.classifier.matches("D"))
        })
        .expect("stable D classifier");
    let constructor = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.owner == Some(d)
                        && header.kind == crate::fir::DeclarationKind::Constructor
                })
        })
        .and_then(|declaration| index.signature(declaration))
        .expect("stable D constructor signature");
    assert_eq!(
        constructor.parameters[0].get(),
        Ty::obj_args(
            "C",
            &[
                Ty::star_projection(Ty::obj("kotlin/Any")),
                Ty::star_projection(Ty::obj("kotlin/Any")),
            ],
        )
    );
}

#[test]
fn compact_symbol_collection_never_dereferences_released_body_arenas() {
    let source = r#"
        annotation class Mark
        class Host(value: Int = 1) {
            init { initialize() }
            val later: Int = 1
            fun initialize() {}
            fun run(): String {
                fun <@Mark T> keep(value: T): T = value
                return keep("OK")
            }
            class Types {
                class Value
                typealias Alias = Value
            }
        }
        fun inferred() = 1
    "#;
    let input = SourceInput::kotlin(source).with_file_stem("ReleasedBodies");
    let mut diagnostics = DiagSink::new();
    let mut file = crate::frontend::parse_source_with_detected_features(source, &mut diagnostics);
    let mut builder = crate::fir::HeaderInventoryBuilder::default();
    builder
        .add_source(0, &input, Some(&file))
        .expect("valid Kotlin source must produce compact headers");
    let headers = builder.finish();

    file.release_body_arenas();
    assert!(file.expr_arena.is_empty());
    assert!(file.stmt_arena.is_empty());
    let symbols = super::collect_signatures_with_cp_headers(
        std::slice::from_ref(&file),
        &headers,
        Box::new(EmptySymbolSource),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(symbols
        .funs
        .get("inferred")
        .is_some_and(|functions| functions.len() == 1 && functions[0].ret == Ty::Pending));
}

#[test]
fn stable_constructor_annotations_are_published_without_occurrence_coordinates() {
    let source = r#"
        annotation class Marker
        @Marker
        class Result @Marker constructor() {
            @Marker constructor(value: Int) : this()
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("ConstructorAnnotations")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("constructor annotations must finalize in Pass 1")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .classifier_header(*declaration)
                .is_some_and(|classifier| classifier.classifier.matches("Result"))
        })
        .expect("stable Result classifier");
    assert_eq!(index.declaration_annotations(result).len(), 1);
    assert!(index.declaration_annotations(result)[0].matches("Marker"));
    let constructors = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.owner == Some(result)
                        && header.kind == crate::fir::DeclarationKind::Constructor
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(constructors.len(), 2);
    for constructor in constructors {
        assert_eq!(index.declaration_annotations(constructor).len(), 1);
        assert!(index.declaration_annotations(constructor)[0].matches("Marker"));
    }
}

#[test]
fn anonymous_subclass_in_if_condition_publishes_its_local_parent() {
    assert_streaming_frontend(
        r#"fun box(): String {
               val captured = "OK"
               open class Local { fun value() = captured }
               if (object : Local() {}.value() == "OK") return "OK"
               return "fail"
           }"#,
        "ConditionAnonymousParent",
    );
}

#[test]
fn anonymous_object_publishes_nested_inner_classifier_headers() {
    assert_streaming_frontend(
        r#"class Generic<T>(val value: T) {
               fun read(): T {
                   val outer = object {
                       inner class First {
                           fun next(): T {
                               val inner = object {
                                   inner class Second { fun value(): T = this@Generic.value }
                               }
                               return inner.Second().value()
                           }
                       }
                   }
                   return outer.First().next()
               }
           }"#,
        "AnonymousNestedInnerHeaders",
    );
}

#[test]
fn nested_local_parent_uses_outer_function_capture_not_nearer_class_formal() {
    assert_streaming_frontend(
        r#"interface Callback { fun foo() }
           interface Values<in T> { fun accept(value: T) }
           interface Combined<in T> : Values<T>, Callback
           interface Observer<in T>
           interface CombinedObserver<in T> : Observer<T>, Combined<T>
           fun <Outer> test(emitter: Combined<Outer>) {
               class Local<Near> : CombinedObserver<Outer>, Callback by emitter {
                   fun enter(value: Outer) {
                       class Nested : CombinedObserver<Outer>, Observer<Outer> by this,
                           Combined<Outer> by emitter
                   }
               }
           }"#,
        "NestedLocalCaptureIdentity",
    );
}

#[test]
fn reified_outer_capture_is_visible_in_anonymous_members() {
    assert_jvm_streaming_frontend(
        r#"// WITH_STDLIB
           interface Marker
           class Box<T>
           private inline fun <reified T> Box<T>.marker() = object : Marker {
               val classifier = T::class
               fun accepts(value: Any): Boolean = value is T
           }
           fun use() { Box<String>().marker() }"#,
        "ReifiedAnonymousCapture",
    );
}

#[test]
fn retained_inline_local_classifier_uses_its_finalized_captured_formal() {
    assert_jvm_streaming_frontend(
        r#"// WITH_STDLIB
           // LANGUAGE: +AllowReifiedTypeInCatchClause
           inline fun <reified E : Throwable> invoke(): String = object {
               inline fun <reified X : Throwable> catchIt(): String = try {
                   throw IllegalStateException("fail")
               } catch (error: X) {
                   "OK"
               }
               fun run(): String = catchIt<IllegalStateException>()
           }.run()
           fun use(): String = invoke<AssertionError>()"#,
        "RetainedReifiedCatchCapture",
    );
}

#[test]
fn nested_generic_member_signature_uses_its_published_type_parameter_identity() {
    let source = r#"// WITH_STDLIB
        import kotlin.reflect.KClass

        interface GraphQlTester {
            interface Entity<D, S : Entity<D, S>>
            interface Path {
                fun <E : Any> entity(entityType: KClass<E>): Entity<E, *>
            }
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedGenericMemberIdentity")];
    let stems = ["NestedGenericMemberIdentity".to_string()];
    let mut paths = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::from_source(source),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("nested generic member signature must finalize in Pass 1")
        .module
        .index();
    let entity = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index.declaration_name(*declaration) == Some("entity")
                && index
                    .declaration_header(*declaration)
                    .and_then(|header| header.owner)
                    .and_then(|owner| index.classifier_header(owner))
                    .is_some_and(|owner| owner.classifier.matches("GraphQlTester$Path"))
        })
        .expect("stable GraphQlTester.Path.entity declaration");
    let parameter = index
        .type_parameter(entity, 0)
        .expect("entity must publish its E type parameter");
    let semantic = index
        .type_parameter_semantic_name(parameter)
        .expect("E must retain its semantic identity");
    let signature = index.signature(entity).expect("stable entity signature");
    let mut referenced = Vec::new();
    for ty in signature
        .parameters
        .iter()
        .map(|ty| ty.get())
        .chain(std::iter::once(signature.result.get()))
    {
        super::ty_param_names_into(ty, &mut referenced);
    }
    assert_eq!(referenced, vec![semantic, semantic], "{signature:?}");
}

fn assert_streaming_frontend(source: &str, stem: &str) {
    let inputs = [SourceInput::kotlin(source).with_file_stem(stem)];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

fn assert_jvm_streaming_frontend(source: &str, stem: &str) {
    let inputs = [SourceInput::kotlin(source).with_file_stem(stem)];
    let stems = [stem.to_string()];
    let mut paths = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::jvm::classpath::platform_jdk_modules(None) {
        paths.push(jdk);
    }
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::from_source(source),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}
