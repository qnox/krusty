use super::*;
use crate::features::LangFeatures;
use crate::frontend::{PlatformLibraries, PlatformProvider};
use crate::lexer::lex;
use crate::parser::parse;

mod annotation_identity;
mod explicit_backing_fields;
mod range_membership;

/// Where NO_VALUE_FOR_PARAMETER is anchored: `argument` in the argument list, or the callee's
/// name where the reference version reports it there. That table row is checked against kotlinc
/// by `tests/diagnostic_wording_versions_e2e.rs`; these tests check the resolver follows it.
fn missing_argument_anchor<'a>(argument: &'a str, callee: &'a str) -> &'a str {
    match crate::diagnostic_wording::no_value_for_parameter_anchor() {
        crate::diagnostic_wording::Anchor::ValueArguments => argument,
        crate::diagnostic_wording::Anchor::ReferencedNameByQualified => callee,
    }
}

fn initialized_jvm_libraries(
    classpath: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::jvm_libraries::JvmLibraries {
    crate::jvm::jvm_libraries::JvmLibraries::new(classpath).expect("JVM provider initialization")
}

fn jvm_platform(libraries: impl Into<PlatformLibraries>) -> PlatformProvider {
    PlatformProvider::jvm(libraries)
}

#[test]
fn package_only_qualified_classifier_failure_reports_the_missing_tail() {
    let result = walk_qualifier_namespace_facets_with_binding_identity(
        &EmptySymbolSource,
        None,
        None,
        "kotlin",
        &[(None, "Missing".to_string())],
    )
    .map(|(resolved, _, _)| resolved);

    assert_eq!(
        result,
        Err(QualifierError::UnresolvedSegment {
            expression: None,
            name: "Missing".to_string(),
        })
    );
}

#[test]
fn local_class_nested_typealias_is_collected_under_its_hoisted_source_owner() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "fun box() { class Local { typealias Alias = Local } }",
        &mut diagnostics,
    );
    let local = file
        .local_class_decls
        .values()
        .copied()
        .next()
        .expect("hoisted local class");
    let Decl::Class(local) = file.decl(local) else {
        panic!("local declaration must be a class")
    };
    let identity = type_name(&format!("{}/Alias", local.name.replace('.', "/")));
    let symbols = collect_signatures(&[file], &mut diagnostics);

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(
        symbols.source_alias_expansions.contains_key(&identity),
        "missing {identity:?}; collected {:?}",
        symbols.source_alias_expansions.keys().collect::<Vec<_>>()
    );
}

#[test]
fn value_class_constructor_keeps_its_semantic_array_parameter() {
    let mut diagnostics = DiagSink::new();
    let jvm_annotations = parse_file(
        "package kotlin.jvm\nannotation class JvmInline",
        &mut diagnostics,
    );
    let file = parse_file(
            "@JvmInline value class UIntArray(private val storage: IntArray) { val size get() = storage.size }",
            &mut diagnostics,
        );
    let symbols = collect_signatures(&[jvm_annotations, file], &mut diagnostics);

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let class = symbols
        .class_by_type_name(type_name("UIntArray"))
        .expect("value-class signature");
    let int_array = Ty::array(Ty::Int);
    assert_eq!(class.ctor_params, [int_array]);
    assert_eq!(class.ctor_param_shapes, [(int_array, false)]);
    assert_eq!(class.value_field, Some(("storage".to_string(), int_array)));
    assert_eq!(class.declared_props["size"].ty, Ty::Int);
}

#[test]
fn production_top_level_signature_types_are_read_from_compact_headers() {
    let source = "// LANGUAGE: +NestedTypeAliases\n\
                      typealias Count = Long\n\
                      typealias Cargo = String\n\
                      @MarkerAnnotation inline fun use(value: Int): String = \"OK\"\n\
                      fun carry(value: Cargo): Cargo = value\n\
                      @MarkerAnnotation val count: Long = 1L\n\
                      var guarded: Long = 1L\n\
                          private set\n\
                      val String.first: Char get() = this[0]\n\
                      interface Marker\n\
                      open class Parent<T>\n\
                      class Box<out T : Marker>(@MarkerAnnotation val value: Int) : Parent<String>() {\n\
                          typealias Alias = String\n\
                          @MarkerAnnotation private inline fun <U : Marker> convert(input: Long): String = \"OK\"\n\
                          fun matches(@kotlin.EqualityBound(Box::class) other: Any?): Boolean = false\n\
                          val label: Long = 1L\n\
                          val inferred = 42\n\
                          val String.suffix: Char get() = this[0]\n\
                          constructor(flag: Boolean) : this(1)\n\
                      }\n\
                      annotation class MarkerAnnotation\n";
    let input = crate::source::SourceInput::kotlin(source).with_file_stem("Compact");
    let mut diagnostics = DiagSink::new();
    let mut file = parse_file(source, &mut diagnostics);
    let mut builder = crate::fir::HeaderInventoryBuilder::default();
    let (source_id, stubs, _) = builder
        .add_source(0, &input, Some(&file))
        .expect("valid Kotlin source must produce compact headers");
    let mut signature_constraints = crate::fir::SignatureConstraintExtractor::default();
    signature_constraints.extract_file(&file, source_id, &stubs, |span| {
        builder.source_origin(source_id, span)
    });
    let headers = builder.finish();

    // Deliberately corrupt the transient declaration header after compact extraction. This is
    // a migration regression: production signature publication must no longer read these AST
    // type nodes, even though the body/default-expression seam still owns the transient file.
    let declaration = file.decls[0];
    let Decl::Fun(function) = file.decl_mut(declaration) else {
        panic!("test source must parse as a function")
    };
    function.params[0].ty.name = "Boolean".to_string();
    function.ret.as_mut().unwrap().name = "Int".to_string();
    function.flags = crate::ast::FdFlags::default();
    function.visibility = Visibility::Private;
    function.annotations.clear();
    function.annotation_args.clear();
    let carry_declaration = file.decls[1];
    let Decl::Fun(carry) = file.decl_mut(carry_declaration) else {
        panic!("test source must contain carry function")
    };
    carry.params[0].ty.name = "Boolean".to_string();
    carry.ret.as_mut().unwrap().name = "Int".to_string();
    file.alias_spellings.clear();
    let count_declaration = file.decls[2];
    let Decl::Property(count) = file.decl_mut(count_declaration) else {
        panic!("test source must contain count property")
    };
    count.ty.as_mut().unwrap().name = "String".to_string();
    count.is_var = true;
    count.is_const = true;
    count.is_lateinit = true;
    count.annotations.clear();
    count.init = None;
    let guarded_declaration = file.decls[3];
    let Decl::Property(guarded) = file.decl_mut(guarded_declaration) else {
        panic!("test source must contain guarded property")
    };
    guarded.setter = None;
    let extension_declaration = file.decls[4];
    let Decl::Property(extension) = file.decl_mut(extension_declaration) else {
        panic!("test source must contain extension property")
    };
    extension.receiver.as_mut().unwrap().name = "Int".to_string();
    extension.ty.as_mut().unwrap().name = "Long".to_string();
    extension.getter = None;
    file.type_alias_fun[0].2.name = "String".to_string();
    file.type_aliases[0].1 = "String".to_string();
    let box_declaration = file
            .decls
            .iter()
            .copied()
            .find(|&declaration| {
                matches!(file.decl(declaration), Decl::Class(class) if class.name == "Box")
            })
            .expect("test source must contain Box");
    let Decl::Class(class) = file.decl_mut(box_declaration) else {
        unreachable!()
    };
    class.visibility = Visibility::Private;
    class.modality = crate::ast::Modality::Abstract;
    class.kind = crate::ast::ClassKind::Interface;
    class.singleton = true;
    class.is_data = true;
    class.is_value = true;
    class.is_fun_interface = true;
    class.props[0].ty.name = "Boolean".to_string();
    class.props[0].visibility = Visibility::Private;
    class.props[0].is_open = true;
    class.props[0].annotations.clear();
    class.base_class = Some("MissingLegacyBase".to_string());
    class.base_type_args[0].name = "Int".to_string();
    let names = class.type_params.clone();
    let mut bounds = class.type_param_bounds.clone();
    bounds[0].1.name = "Any".to_string();
    let variances = class.type_param_variances.clone();
    class.type_parameters.replace(names, bounds, variances);
    class.methods[0].params[0].ty.name = "Boolean".to_string();
    class.methods[0].ret.as_mut().unwrap().name = "Int".to_string();
    class.methods[0].type_param_bounds[0].1.name = "Any".to_string();
    class.methods[0].visibility = Visibility::Public;
    class.methods[0].flags = crate::ast::FdFlags::default();
    class.methods[0].annotations.clear();
    class.methods[0].annotation_args.clear();
    class.methods.clear();
    class.body_props[0].ty.as_mut().unwrap().name = "Boolean".to_string();
    class.body_props[1].init = None;
    class.body_props[2].receiver.as_mut().unwrap().name = "Int".to_string();
    class.body_props[2].ty.as_mut().unwrap().name = "Long".to_string();
    class.body_props[2].getter = None;
    class.body_props.clear();
    class.secondary_ctors[0].params[0].ty.name = "String".to_string();
    class.type_aliases.clear();

    let files = [file];
    let mut symbols = collect_signatures_with_cp_headers(
        &files,
        &headers,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let signature = &symbols.funs["use"][0];
    assert_eq!(signature.params, [Ty::Int]);
    assert_eq!(signature.ret, Ty::String);
    assert!(signature.is_inline());
    assert_eq!(signature.visibility, Visibility::Public);
    assert_eq!(signature.annotations, [type_name("MarkerAnnotation")]);
    let carry = &symbols.funs["carry"][0];
    assert_eq!(carry.params, [Ty::String]);
    assert_eq!(carry.ret, Ty::String);
    let count = &symbols.source_props[&(0, count_declaration.0)];
    assert_eq!(count.ty, Ty::Long);
    assert!(!count.is_var);
    assert!(!count.is_const);
    assert!(!count.is_lateinit);
    assert_eq!(count.annotations, [type_name("MarkerAnnotation")]);
    assert_eq!(
        symbols.source_props[&(0, guarded_declaration.0)].setter_visibility,
        Visibility::Private
    );
    let extension = symbols
        .ext_props
        .values()
        .flatten()
        .find(|property| property.source == (0, extension_declaration.0))
        .expect("compact extension property signature");
    assert_eq!(extension.receiver, Ty::String);
    assert_eq!(extension.ty, Ty::Char);
    assert_eq!(
        symbols.source_alias_expansions[&type_name("Count")].expansion,
        Ty::Long
    );
    assert_eq!(
        symbols.source_alias_expansions[&type_name("Box/Alias")].expansion,
        Ty::String
    );
    let class = symbols
        .class_by_type_name(type_name("Box"))
        .expect("compact classifier signature");
    let bootstrap = symbols
        .source_class_headers
        .get(&type_name("Box"))
        .expect("compact classifier bootstrap");
    assert_eq!(
        bootstrap.direct_supertypes.iter_ids().collect::<Vec<_>>(),
        [type_name("Parent")]
    );
    assert_eq!(class.visibility, Visibility::Public);
    assert!(class.flags.has(ClassFlags::FINAL));
    assert!(!class.flags.has(ClassFlags::ABSTRACT));
    assert!(!class.flags.has(ClassFlags::INTERFACE));
    assert!(!class.flags.has(ClassFlags::OBJECT));
    assert!(!class.flags.has(ClassFlags::FUN_INTERFACE));
    assert!(class.value_field.is_none());
    assert!(!class.methods.contains_key("component1"));
    assert!(!class.methods.contains_key("copy"));
    assert!(symbols.enum_entries_of(type_name("Box")).is_none());
    assert_eq!(class.ctor_params, [Ty::Int]);
    let value = &class.declared_props["value"];
    assert_eq!(value.visibility, Visibility::Public);
    assert!(!value.is_open);
    assert_eq!(value.annotations, [type_name("MarkerAnnotation")]);
    assert!(value.stable_declaration.is_some());
    assert!(value.source_member.is_none());
    assert_eq!(class.super_type_args, [Ty::String]);
    assert_eq!(class.type_parameters.type_param_bounds, [Ty::obj("Marker")]);
    assert_eq!(
        class.type_parameters.type_param_variances,
        [crate::types::TypeVariance::Out]
    );
    let method = &class.methods["convert"][0];
    assert_eq!(method.params, [Ty::Long]);
    assert_eq!(method.ret, Ty::String);
    assert_eq!(method.visibility, Visibility::Private);
    assert!(method.is_inline());
    assert_eq!(method.annotations, [type_name("MarkerAnnotation")]);
    assert!(method.stable_declaration.is_some());
    assert!(method.source_member.is_none());
    let matches = &class.methods["matches"][0];
    assert_eq!(matches.equality_bound, Some(Ty::obj("Box")));
    assert_eq!(
        method
            .generic_sig
            .as_ref()
            .expect("compact generic member signature")
            .formal_bounds,
        [vec![Ty::obj("Marker")]],
    );
    assert_eq!(class.declared_props["label"].ty, Ty::Long);
    assert_eq!(class.declared_props["inferred"].ty, Ty::Pending);
    let extension = &class.member_ext_props["suffix"][0];
    assert_eq!(extension.receiver, Ty::String);
    assert_eq!(extension.ret, Ty::Char);
    assert_eq!(class.secondary_ctors, [vec![Ty::Boolean]]);

    streaming_signature_tests::assert_finalized_carry_spellings(
        &headers,
        &mut symbols,
        signature_constraints,
        &mut diagnostics,
    );
}

#[test]
fn production_supertype_cycle_diagnostics_use_compact_header_origins() {
    let source = "interface A : B\ninterface B : A";
    let input = crate::source::SourceInput::kotlin(source).with_file_stem("Cycle");
    let mut diagnostics = DiagSink::new();
    let mut file = parse_file(source, &mut diagnostics);
    let mut builder = crate::fir::HeaderInventoryBuilder::default();
    builder.add_source(0, &input, Some(&file));
    let headers = builder.finish();
    let expected = headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == crate::fir::DeclarationKind::Classifier)
        .filter_map(|stub| {
            let crate::fir::HeaderDeclarationKind::Classifier { supertypes, .. } =
                headers.syntax.declaration(stub.id)?.kind
            else {
                return None;
            };
            headers
                .syntax
                .type_operands(supertypes)
                .first()
                .and_then(|supertype| headers.syntax.ty(*supertype))
                .map(|ty| ty.span)
        })
        .collect::<Vec<_>>();

    // Destroy the legacy edge locations after compact extraction. Both the semantic graph and
    // its diagnostics must now be entirely header-owned in the production collector.
    for declaration in file.decls.clone() {
        if let Decl::Class(class) = file.decl_mut(declaration) {
            class.supertypes.clear();
        }
    }
    diagnostics.diags.clear();
    let _ = collect_signatures_with_cp_headers(
        &[file],
        &headers,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| (diagnostic.span, diagnostic.msg.as_str()))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .copied()
            .map(|span| (
                span,
                "cycle in supertypes and/or containing declarations detected."
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn source_aliases_do_not_break_internal_class_table_keys() {
    // A source alias is a file-scoped binding edge to `sample/Record`, never a second class entry
    // keyed by the alias spelling. This invariant lets direct lookup, hierarchy traversal, module
    // symbol projection, and lowering all use the same map without an alias-only branch.
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "package sample\nclass Record(val value: Int)\ntypealias LocalRecord = Record",
        &mut diagnostics,
    );
    let symbols = collect_signatures(&[file], &mut diagnostics);

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(symbols
        .classes
        .iter()
        .all(|(internal, signature)| *internal == signature.internal_name()));
    // The forward-reference header index is keyed by those same identities and is generated by
    // the same flag function as the completed signature; it must never become a second, divergent
    // classification table while making source-order-independent inference possible.
    assert!(symbols.classes.iter().all(|(internal, signature)| {
        symbols
            .source_class_headers
            .get(internal)
            .is_some_and(|header| header.flags == signature.flags)
    }));
    assert_eq!(
        symbols
            .source_class_alias(0, "LocalRecord")
            .map(ClassSig::internal_name),
        Some(type_name("sample/Record"))
    );
    assert!(!symbols.classes.contains_key(&type_name("LocalRecord")));
}

#[test]
fn receiver_lambda_expectation_survives_an_erased_provider_parameter() {
    // A provider may carry the semantic receiver only in CallSig metadata while its physical
    // parameter is an opaque callable object (no JVM/generic function signature). The checker
    // must bind `this` from the origin-neutral call contract and must not recognize a concrete
    // runtime callable class name to do so.
    let erased_callable = Ty::obj("fixture/ErasedCallable");
    let mut candidate = crate::libraries::LibraryMember::new(
        "apply".to_string(),
        vec![erased_callable],
        Ty::Unit,
        "(Ljava/lang/Object;)V".to_string(),
    );
    candidate.call_sig = CallSig::metadata_function(
        1,
        vec!["block".to_string()],
        vec![false],
        vec![Some(Ty::String)],
        vec![true],
        vec![crate::types::InlineParameterModifier::None],
        None,
    );

    let expectation = functional_argument_expectation(
        &crate::libraries::EmptySymbolSource,
        &crate::libraries::EmptySymbolSource,
        &candidate.call_sig,
        false,
        0,
        erased_callable,
        None,
    )
    .expect("CallSig receiver metadata must survive erased provider parameters");
    assert_eq!(expectation.receiver, Some(Ty::String));
    assert!(expectation.value_params.is_empty());
}

#[test]
fn capture_discovery_selects_only_enclosing_top_level_declarations() {
    let mut diagnostics = DiagSink::new();
    let source = r#"
interface Marker
fun before(): Int = 1
val top: Marker = object : Marker { fun value(): Int = 4 }
fun defaulted(value: Marker = object : Marker {}): Marker = value
fun target(seed: Int): Marker {
    val captured = seed
    return object : Marker { fun value(): Int = captured }
}
class MethodHost {
    fun declared(): Int = 2
    fun inferred() = source()
    override fun toString() = source()
    fun block(): Int { return 2 }
    fun build(seed: Int): Marker {
        val captured = seed
        return object : Marker { fun value(): Int = captured }
    }
    fun defaulted(value: Marker = object : Marker {}): Marker = value
    fun after(): Int = 3
}
class PropertyHost {
    fun declared(): Int = 2
    fun inferred() = source()
    override fun toString() = source()
    fun block(): Int { return 2 }
    val target: Marker = object : Marker { fun value(): Int = 4 }
    fun laterInferred() = source()
}
class CompanionHost {
    fun dependency() = source()
    fun declared(): Int = 5
    companion object {
        fun untouched(): Int = 6
        fun build(seed: Int): Marker {
            val captured = seed
            return object : Marker { fun value(): Int = captured }
        }
    }
}
fun after(): Int = 3
"#;
    let tokens = lex(source, &mut diagnostics);
    let file = parse(source, &tokens, &mut diagnostics);
    assert!(diagnostics.diags.is_empty());

    let selected = capture_discovery_scope(&file).declarations;
    let named = file
        .decls
        .iter()
        .filter_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if selected.contains(declaration) => Some(function.name.as_str()),
            Decl::Class(class) if selected.contains(declaration) => Some(class.name.as_str()),
            Decl::Property(property) if selected.contains(declaration) => {
                Some(property.name.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        named,
        vec![
            "top",
            "defaulted",
            "target",
            "MethodHost",
            "PropertyHost",
            "CompanionHost.Companion"
        ]
    );

    let class = |name: &str| {
        file.decls
            .iter()
            .find_map(|declaration| match file.decl(*declaration) {
                Decl::Class(class) if class.name == name => Some((*declaration, class)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("source should contain {name}"))
    };
    let scope = capture_discovery_scope(&file);
    let (method_host, _) = class("MethodHost");
    let method_plan = scope
        .class_plans
        .get(&method_host)
        .expect("selected class should have a complete plan");
    // The last target is in a parameter default, proving the generic function-root seam is not
    // a body-only shortcut. Within that prefix, only an inferred-return publisher and the two
    // target owners require semantic checking; declared, block-body, and fixed-contract methods
    // retain only the cheap mutation-summary transition. The unrelated suffix is omitted.
    assert_eq!(
        method_plan.methods,
        vec![false, true, false, false, true, true]
    );

    let (property_host, _) = class("PropertyHost");
    let property_plan = scope
        .class_plans
        .get(&property_host)
        .expect("selected class should have a complete plan");
    // A property target retains the complete ordered method sequence, including a publisher
    // after the property. This is intentionally conservative because the checker processes
    // method return publication before later class regions.
    assert_eq!(property_plan.methods, vec![false, true, false, false, true]);

    let (companion_host, _) = class("CompanionHost.Companion");
    let companion_plan = scope
        .class_plans
        .get(&companion_host)
        .expect("selected class should have a complete plan");
    assert_eq!(companion_plan.methods, vec![false, true]);
}

#[test]
fn capture_discovery_uses_generic_declaration_expression_roots() {
    let mut diagnostics = DiagSink::new();
    let source = r#"
interface Marker
open class Base(val marker: Marker)
fun before(): Int = 1
class Primary(val marker: Marker = object : Marker {})
class Derived : Base(object : Marker {})
class Delegated : Marker by object : Marker {}
class Container {
    init { val initialized = object : Marker {} }
    val member = object : Marker {}
    companion object : Base(object : Marker {}) {
        fun make(): Marker = object : Marker {}
        val value = object : Marker {}
    }
    constructor(marker: Marker = object : Marker {}) {
        val constructed = object : Marker {}
    }
}
enum class Choice(val marker: Marker) {
    FIRST(object : Marker {})
}
val custom: Marker get() = object : Marker {}
val delegated: Marker by object {
    operator fun getValue(owner: Any?, property: Any?): Marker = object : Marker {}
}
fun after(): Int = 2
"#;
    let tokens = lex(source, &mut diagnostics);
    let file = parse(source, &tokens, &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let selected = capture_discovery_scope(&file).declarations;
    let synthesized = file
        .anonymous_object_classes
        .values()
        .copied()
        .collect::<crate::fx_hash::HashSet<_>>();
    let named = file
        .decls
        .iter()
        // A construction inside an anonymous object's member correctly selects that synthesized
        // class too. Compare only source-level owners here so the regression neither depends on
        // nor prints parser-generated internal class names.
        .filter(|declaration| !synthesized.contains(declaration))
        .filter_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if selected.contains(declaration) => Some(function.name.as_str()),
            Decl::Class(class) if selected.contains(declaration) => Some(class.name.as_str()),
            Decl::Property(property) if selected.contains(declaration) => {
                Some(property.name.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    // `before`, `after`, `Marker`, and `Base` are sentinels: if a future parser field falls
    // outside `File::any_decl_expr`, completeness detection selects every declaration and makes
    // this assertion fail instead of silently losing capture information.
    assert_eq!(
        named,
        vec![
            "Primary",
            "Derived",
            "Delegated",
            "Container",
            "Container.Companion",
            "Choice",
            "custom",
            "delegated",
        ]
    );
}

fn check(src: &str) -> (Vec<String>, Option<TypeInfo>) {
    let mut d = DiagSink::new();
    let (_, _, info) = crate::frontend::analyze_source_standalone(src, &mut d);
    let errs: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
    (errs, info)
}

fn retained_standalone_analysis(src: &str) -> (File, TypeInfo, DiagSink) {
    let mut diagnostics = DiagSink::new();
    let (file, _, info) = crate::frontend::analyze_source_standalone(src, &mut diagnostics);
    let info = info.expect("production frontend must retain checked standalone analysis");
    (file, info, diagnostics)
}

fn retained_platform_analysis(
    src: &str,
    platform: Box<dyn crate::libraries::SemanticPlatform>,
) -> (File, TypeInfo, DiagSink) {
    let mut diagnostics = DiagSink::new();
    let (file, _, info) =
        crate::frontend::analyze_source(src, jvm_platform(platform), &mut diagnostics);
    let info = info.expect("production frontend must retain checked platform analysis");
    (file, info, diagnostics)
}

fn retained_standalone_source_set(sources: &[&str]) -> (Vec<File>, Vec<TypeInfo>, DiagSink) {
    retained_standalone_source_set_with_prepare(sources, |_, _| {})
}

fn retained_standalone_source_set_with_prepare(
    sources: &[&str],
    prepare_symbols: impl FnOnce(&[File], &mut SymbolTable),
) -> (Vec<File>, Vec<TypeInfo>, DiagSink) {
    let mut diagnostics = DiagSink::new();
    let inputs = sources
        .iter()
        .map(|source| crate::frontend::SourceInput::kotlin(source))
        .collect::<Vec<_>>();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare_retained(
        &inputs,
        jvm_platform(Box::new(crate::libraries::EmptySymbolSource)),
        &LangFeatures::new(),
        prepare_symbols,
        &mut diagnostics,
    );
    let types = analysis
        .types
        .into_iter()
        .map(|info| info.expect("production frontend must check every source file"))
        .collect();
    (analysis.files, types, diagnostics)
}

fn check_with_annotation_fixtures(
    src: &str,
    detected_features: bool,
) -> (Vec<String>, Option<TypeInfo>) {
    let mut diagnostics = DiagSink::new();
    let kotlin_annotations = "package kotlin\n\
             annotation class Suppress(vararg val names: String)";
    let exact_annotation = "package kotlin.internal\nannotation class Exact";
    let jvm_annotations = "package kotlin.jvm\nannotation class JvmInline";
    let source = if detected_features {
        src.to_owned()
    } else {
        // The production frontend always detects source directives. Preserve the old helper's
        // no-directive mode by removing only LANGUAGE directives from its synthetic fixture.
        src.lines()
            .filter(|line| !line.trim_start().starts_with("// LANGUAGE:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let sources = [
        kotlin_annotations,
        exact_annotation,
        jvm_annotations,
        source.as_str(),
    ];
    let mut analysis = crate::frontend::analyze_source_set(
        &sources,
        jvm_platform(Box::new(crate::libraries::EmptySymbolSource)),
        &mut diagnostics,
    );
    let info = analysis.types.get_mut(3).and_then(Option::take);
    let errors = diagnostics
        .diags
        .iter()
        .map(|diagnostic| diagnostic.msg.clone())
        .collect();
    (errors, info)
}

fn check_with_detected_features(src: &str) -> Vec<String> {
    let mut diagnostics = DiagSink::new();
    let _ = crate::frontend::analyze_source_standalone(src, &mut diagnostics);
    diagnostics
        .diags
        .iter()
        .map(|diagnostic| diagnostic.msg.clone())
        .collect()
}

#[test]
fn visibility_suppression_uses_the_lexical_annotation_identity() {
    let (file_errors, _) = check_with_annotation_fixtures(
        r#"
@file:KotlinSuppress("INVISIBLE_REFERENCE")
import kotlin.Suppress as KotlinSuppress

class FileOwner { private fun hidden(): Int = 1 }
fun fileAllowed(owner: FileOwner): Int = owner.hidden()
"#,
        false,
    );
    assert_eq!(file_errors, Vec::<String>::new());

    let (errors, _) = check_with_annotation_fixtures(
        r#"
// LANGUAGE: +ExplicitBackingFields
import kotlin.Suppress as KotlinSuppress

annotation class Suppress(vararg val names: String)
class Owner { private fun hidden(): Int = 1 }

@KotlinSuppress("INVISIBLE_REFERENCE")
fun allowed(owner: Owner) { owner.hidden() }

class MemberProperty {
    @KotlinSuppress("INVISIBLE_REFERENCE")
    val value: Int = Owner().hidden()
}

class Primary @KotlinSuppress("INVISIBLE_REFERENCE") constructor(
    val value: Int = Owner().hidden(),
)

class Secondary {
    @KotlinSuppress("INVISIBLE_REFERENCE")
    constructor() { Owner().hidden() }
}

fun outer() {
    @KotlinSuppress("INVISIBLE_REFERENCE")
    fun local(): Int = Owner().hidden()
    local()

    class LocalInferred {
        @KotlinSuppress("INVISIBLE_REFERENCE")
        val value = Owner().hidden()
    }
    LocalInferred().value

    class LocalBacking {
        @KotlinSuppress("INVISIBLE_REFERENCE")
        val value: Any
            field = Owner().hidden()
    }
    LocalBacking().value
}

@Suppress("INVISIBLE_REFERENCE")
fun rejected(owner: Owner) { owner.hidden() }
"#,
        true,
    );

    assert_eq!(
        errors,
        ["cannot access 'fun hidden(): Int': it is private in 'Owner'."]
    );
}

#[test]
fn bare_type_parameter_does_not_admit_null_from_its_upper_bound() {
    for source in [
        "fun <T> take(value: T) {}\nfun <T> invalid() { take<T>(null) }",
        "fun <T> invalid(): T { val value: T = null; return value }",
    ] {
        let (errors, _) = check(source);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("null cannot be a value of a non-null type")),
            "{source}: {errors:?}"
        );
    }
}

#[test]
fn invariant_ordinary_argument_fixes_a_later_lambda_result() {
    let (errors, _) = check(
        "class Holder<T>\n\
             fun <T> choose(holder: Holder<T>, block: () -> T): T = block()\n\
             fun invalid(holder: Holder<String>): String = choose(holder) { Any() }",
    );

    assert!(
        errors.iter().any(|error| {
            error.contains("type mismatch") && error.contains("String") && error.contains("Any")
        }),
        "an invariant Holder<String> must fix T before the lambda is checked: {errors:?}"
    );
}

#[test]
fn expected_constructor_type_arguments_must_satisfy_class_bounds() {
    let (valid, _) = check(
        "open class Bound\n\
             class Unrelated\n\
             interface I<T>\n\
             class C<T : Bound> : I<T>\n\
             fun valid(): I<Bound> = C()",
    );
    assert!(
        valid.is_empty(),
        "bounded expected inference should succeed: {valid:?}"
    );

    let (invalid, _) = check(
        "open class Bound\n\
             class Unrelated\n\
             interface I<T>\n\
             class C<T : Bound> : I<T>\n\
             fun invalid(): I<Unrelated> = C()",
    );
    assert!(
        !invalid.is_empty(),
        "an expected I<Unrelated> must not construct the invalid C<Unrelated>"
    );
}

#[test]
fn data_class_components_come_only_from_primary_constructor_properties() {
    let errors = check_with_detected_features(
        "data class Box(val primary: String) { val body: String = \"body\" }\n\
             fun read(box: Box): String = box.component2()",
    );

    assert_eq!(
        errors,
        [crate::diagnostic_wording::unresolved_reference_on(
            "component2",
            Some("Box")
        )]
    );
}

#[test]
fn nested_class_self_label_uses_the_simple_declaration_name() {
    let (errors, _) = check(
        "inline fun <T> T.apply(block: T.() -> Unit): T { block(); return this }\n\
             class Outer {\n\
                 inner class NestedReceiver {\n\
                     init {\n\
                         \"receiver\".apply { this@NestedReceiver }\n\
                     }\n\
                 }\n\
             }",
    );

    assert!(
        errors.is_empty(),
        "nested class self-label should resolve: {errors:?}"
    );
}

#[test]
fn anonymous_object_records_its_lexical_source_class_owner() {
    let source = "open class Base(value: Any)\n\
                      class Outer {\n\
                      \u{20} fun build() = object : Base(Inner()) {}\n\
                      \u{20} private inner class Inner\n\
                      }";
    let mut diagnostics = DiagSink::new();
    let tokens = lex(source, &mut diagnostics);
    let file = parse(source, &tokens, &mut diagnostics);
    let scope = anonymous_lexical_class_scope(&file);
    let (&anonymous, &owner) = scope.owners.iter().next().expect("anonymous owner");
    assert!(file
        .anonymous_object_classes
        .values()
        .any(|id| *id == anonymous));
    assert!(matches!(file.decl(owner), Decl::Class(class) if class.name == "Outer"));
}

#[test]
fn anonymous_member_body_does_not_inherit_enclosing_super_call_this_restriction() {
    let source = "interface Callback { fun invoke(): String }\n\
                      open class Base(val callback: Callback)\n\
                      class Outer {\n\
                      \u{20} val ok = \"OK\"\n\
                      \u{20} inner class Inner : Base(\n\
                      \u{20}   object : Callback {\n\
                      \u{20}     override fun invoke() =\n\
                      \u{20}       (object : Callback {\n\
                      \u{20}         override fun invoke() = ok\n\
                      \u{20}       }).invoke()\n\
                      \u{20}   }\n\
                      \u{20} )\n\
                      }";
    let (_, info, diagnostics) = retained_standalone_analysis(source);

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let captures = info
        .anonymous_object_captures_by_class
        .values()
        .flatten()
        .collect::<Vec<_>>();
    assert!(
        captures
            .iter()
            .any(|capture| capture.ty == Ty::obj("Outer")),
        "the nested object must retain the receiver selected for `ok`: {captures:?}"
    );
    assert!(
            captures
                .iter()
                .all(|capture| capture.ty != Ty::obj("Outer$Inner")),
            "an anonymous class's own initialized body must not see the enclosing unavailable receiver: {captures:?}"
        );
}

#[test]
fn anonymous_object_does_not_retain_a_type_parameter_shadowed_by_a_local_classifier() {
    let source = "fun <T> build(): Any {\n\
                      \u{20} class T\n\
                      \u{20} return object {}\n\
                      }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    let function_start = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Fun(function) if function.name == "build" => Some(function.signature_span.lo),
            _ => None,
        })
        .expect("generic function");
    let anonymous_start = file
        .anonymous_object_classes
        .values()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class) => Some(class.span.lo),
            _ => None,
        })
        .expect("anonymous object");
    assert_no_diags(&diagnostics);
    let function_parameters = info.resolved_declaration_type_parameters(function_start);
    let anonymous_parameters = info.resolved_declaration_type_parameters(anonymous_start);
    assert_eq!(function_parameters.len(), 1);
    assert!(anonymous_parameters.is_empty());
}

#[test]
fn reified_class_literal_handoff_keeps_the_declaration_parameter_identity() {
    // A same-named classifier makes source spelling especially unsafe here: `T::class` in the
    // function body denotes the function's declaration-owned type parameter, not class `T`.
    // Lowering must receive that semantic identity directly instead of an `Obj("T")` marker it
    // would have to reinterpret by spelling.
    let source = "class T\ninline fun <reified T : Any> literal() = T::class";
    let platform = initialized_jvm_libraries(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(Vec::new()),
    ));
    let (file, info, diagnostics) = retained_platform_analysis(source, Box::new(platform));
    let literal = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| {
            matches!(expression, Expr::CallableRef { name, .. } if name == "class")
                .then_some(ExprId(index as u32))
        })
        .expect("class-literal expression");
    let declaration_start = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Fun(function) if function.name == "literal" => Some(function.signature_span.lo),
            _ => None,
        })
        .expect("generic function declaration");
    assert_no_diags(&diagnostics);
    let [semantic] = info.resolved_declaration_type_parameters(declaration_start) else {
        panic!("one declaration-owned type-parameter identity expected");
    };
    let Some(TyKind::TyParam(actual, _)) = info.expr_lowers.get(&literal).and_then(|lowering| {
        let ExprLowering::ClassLiteral { unbound } = lowering else {
            return None;
        };
        unbound.map(|ty| ty.kind())
    }) else {
        panic!(
            "class literal must carry a semantic TyParam, got {:?}",
            info.expr_lowers.get(&literal)
        );
    };
    assert_eq!(actual, semantic.as_str());
    assert_ne!(actual, "T", "semantic identity must not be source spelling");
}

#[test]
fn generic_member_result_keeps_the_enclosing_parameter_identity() {
    // The member's `T` is inferred from `KClass<T>` using the extension's declaration-owned T.
    // If that result degrades to Obj("T"), common lowering can only avoid a bogus `checkcast T`
    // by recovering semantics from source spelling.
    let source = "import kotlin.reflect.KClass\n\
            interface Core { fun <T : Any> getFor(id: String, type: KClass<T>): T }\n\
            inline fun <reified T : Any> Core.getFor(id: String): T = getFor(id, T::class)";
    let platform =
        initialized_jvm_libraries(std::rc::Rc::new(crate::toolchain::stdlib_classpath()));
    let (file, info, diagnostics) = retained_platform_analysis(source, Box::new(platform));
    let call = file
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(index, expression)| match expression {
                Expr::Call { callee, .. }
                    if matches!(file.expr(*callee), Expr::Name(name) if name == "getFor") =>
                {
                    Some(ExprId(index as u32))
                }
                _ => None,
            })
            .expect("member getFor call");
    let declaration_start = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Fun(function) if function.receiver.is_some() => Some(function.signature_span.lo),
            _ => None,
        })
        .expect("generic extension declaration");
    assert_no_diags(&diagnostics);
    let [semantic] = info.resolved_declaration_type_parameters(declaration_start) else {
        panic!("one declaration-owned type-parameter identity expected");
    };
    let TyKind::TyParam(actual, _) = info.semantic_ty(call).kind() else {
        panic!(
            "member result must stay a type parameter, got {:?}",
            info.semantic_ty(call)
        );
    };
    assert_eq!(actual, semantic.as_str());
}

#[test]
fn nested_class_outer_label_uses_the_simple_declaration_name() {
    let (errors, _) = check(
        "inline fun <T> T.apply(block: T.() -> Unit): T { block(); return this }\n\
             class Outer {\n\
                 inner class Middle {\n\
                     inner class Leaf {\n\
                         init {\n\
                             \"receiver\".apply { this@Middle }\n\
                         }\n\
                     }\n\
                 }\n\
             }",
    );

    assert!(
        errors.is_empty(),
        "nested class outer label should resolve: {errors:?}"
    );
}

fn ok(src: &str) {
    let (errs, _) = check(src);
    assert!(
        errs.is_empty(),
        "unexpected errors: {errs:?}\nsource:\n{src}"
    );
}

fn oks(src: &str) {
    let (errors, _) = check_with_annotation_fixtures(src, false);
    assert!(
        errors.is_empty(),
        "unexpected errors: {errors:?}\nsource:\n{src}"
    );
}

#[test]
fn postponed_receiver_lambda_constraints_specialize_the_outer_call() {
    oks(r#"
class Buildee<T> { fun yield(value: T) {} }
class Value
fun <T> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use(value: Value) {
    val built = build { yield(value) }
    exact<Buildee<Value>>(built)
}
"#);
}

#[test]
fn nested_calls_of_the_same_builder_own_distinct_inference_variables() {
    oks(r#"
class Sink<T> { fun send(value: T) {} }
class Flow<T>
fun <T> channel(block: Sink<T>.() -> Unit): Flow<T> = Flow<T>()
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() {
    val outer = channel {
        val inner = channel { send(1) }
        send(inner)
    }
    exact<Flow<Flow<Int>>>(outer)
}
"#);
}

#[test]
fn bare_extension_receiver_property_keeps_the_applied_class_parameter() {
    ok(r#"
class Content<out T>(val value: T)
fun <T> consume(value: T) {}
fun <T> Content<T>.read() { consume<T>(value) }
"#);
}

#[test]
fn postponed_receiver_lambda_preserves_an_enclosing_class_type_parameter() {
    oks(r#"
class Buildee<CT> {
    fun yield(value: CT) {}
    fun materialize(): CT = null as CT
}
fun <FT> build(block: Buildee<FT>.() -> Unit): Buildee<FT> = Buildee<FT>()
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
private fun <T> checkExactType(value: @kotlin.internal.Exact T) {}
class Context<T> {
    fun use(value: T) {
        val built = build { yield(value) }
        checkExactType<Buildee<T>>(built)
    }
    fun produce() {
        fun consume(value: T) {}
        val built = build { consume(materialize()) }
        checkExactType<Buildee<T>>(built)
    }
}
"#);
}

#[test]
fn deeper_inner_class_keeps_outer_property_captured_type_parameter_identity() {
    oks(r#"
class Foo<T> {
    inner class Inner(val value: T) {
        inner class Deeper<K>(val other: K)
    }
}
"#);
}

#[test]
fn postponed_receiver_lambda_preserves_an_applied_enclosing_class_type() {
    oks(r#"
class Buildee<CT> {
    fun yield(value: CT) {}
    fun materialize(): CT = Context<Marker>() as CT
}
class Marker
fun <T> T.apply(block: T.() -> Unit): T = this
fun <FT> build(block: Buildee<FT>.() -> Unit): Buildee<FT> = Buildee<FT>().apply(block)
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
private fun <T> exact(value: @kotlin.internal.Exact T) {}
class Context<T> {
    fun consumeCase() {
        val value: Context<T> = this
        val built = build { yield(value) }
        exact<Buildee<Context<T>>>(built)
        val fromThis = build { yield(this@Context) }
        exact<Buildee<Context<T>>>(fromThis)
    }
    fun produceCase() {
        fun consume(value: Context<T>) {}
        val built = build { consume(materialize()) }
        exact<Buildee<Context<T>>>(built)
        fun nested() {
            fun <T> shareTypeInfo(from: T, to: T) {}
            val shared = build { shareTypeInfo(this@Context, materialize()) }
            exact<Buildee<Context<T>>>(shared)
        }
        nested()
    }
}
"#);
}

#[test]
fn postponed_receiver_lambda_preserves_cast_local_class_parameter() {
    oks(r#"
class UserKlass
class Buildee<CT> {
    fun yield(arg: CT) {}
    fun materialize(): CT = UserKlass() as CT
}
fun <FT> build(instructions: Buildee<FT>.() -> Unit): Buildee<FT> = Buildee<FT>()
class Context<T> {
    fun consumeCase() {
        val arg: T = UserKlass() as T
        val buildee = build { yield(arg) }
        checkExactType<Buildee<T>>(buildee)
    }
    fun produceCase() {
        fun consume(arg: T) {}
        val buildee = build { consume(materialize()) }
        checkExactType<Buildee<T>>(buildee)
    }
}
fun box(): String {
    with(Context<UserKlass>()) {
        consumeCase()
        produceCase()
    }
    return "OK"
}
fun <T> with(value: T, block: T.() -> Unit): T = value
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
private fun <T> checkExactType(value: @kotlin.internal.Exact T) {}
"#);
}

#[test]
fn postponed_lambda_upper_constraints_choose_the_narrowest_type() {
    oks(r#"
open class Base
class Value : Base()
class Builder<T>
fun <T> buildBoth(
    first: Builder<T>.(T) -> Unit,
    second: Builder<T>.(T) -> Unit,
): Builder<T> = Builder<T>()
fun takeBase(value: Base) {}
fun takeValue(value: Value) {}
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() {
    val built = buildBoth(
        { takeBase(it) },
        { takeValue(it) },
    )
    exact<Builder<Value>>(built)
}
"#);
}

#[test]
fn postponed_lambda_lower_constraint_specializes_a_later_member_result() {
    ok(r#"
class Value
class Other
class Builder<T> {
    fun set(value: T) {}
    fun get(): T = null as T
}
fun <T> build(block: Builder<T>.() -> Unit): Builder<T> = Builder<T>()
fun consume(value: Value) {}
fun consume(value: Other) {}
fun use() {
    build {
        set(Value())
        consume(get())
    }
}
"#);
}

#[test]
fn postponed_receiver_lambda_infers_nullable_nothing_from_null() {
    oks(r#"
class Builder<T> { fun yield(value: T) {} }
fun <T> build(block: Builder<T>.() -> Unit): Builder<T> = Builder<T>()
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() {
    val built = build { yield(null) }
    exact<Builder<Nothing?>>(built)
}
"#);
}

#[test]
fn generic_nullable_function_property_assignment_keeps_the_class_parameter() {
    ok(r#"
class Input
class Holder<T> {
    private var callback: ((Input) -> T)? = null
    fun set(callback: (Input) -> T) {
        this.callback = callback
    }
}
"#);
}

#[test]
fn postponed_receiver_constraint_survives_member_extension_selection() {
    ok(r#"
open class Base
class Value : Base()
class Other
class Builder<T : Base> { fun set(value: T) {} }
fun Builder<Value>.set(value: Other) {}
fun <T : Base> build(block: Builder<T>.() -> Unit): Builder<T> = Builder<T>()
fun use() {
    build {
        set(Value())
        set(Other())
    }
}
"#);
}

#[test]
fn generic_constructor_literal_uses_the_type_parameter_bound() {
    ok(r#"
class Box<T : Long>(val value: T)
fun <T : Long> Box<T>.consume(block: Box<T>.() -> Unit) {}
fun use() {
    Box(6).consume { value }
}
"#);
}

#[test]
fn inapplicable_nominal_member_does_not_hide_a_lambda_extension() {
    ok(r#"
interface Sink<T> { fun emit(value: T) }
interface Stream<T> { fun collect(sink: Sink<T>) }
fun <T> Stream<T>.collect(action: (T) -> Unit) {}
fun use(stream: Stream<String>) {
    stream.collect { value -> value.length }
}
"#);
}

#[test]
fn inapplicable_nominal_member_does_not_hide_a_suspend_lambda_extension() {
    ok(r#"
interface Sink<T> { suspend fun emit(value: T) }
interface Stream<T> { suspend fun collect(sink: Sink<T>) }
suspend fun <T> Stream<T>.collect(action: suspend (T) -> Unit) {}
suspend fun use(stream: Stream<String>) {
    stream.collect { value -> value.length }
}
"#);
}

#[test]
fn fun_interface_ignores_inherited_concrete_accessors() {
    ok(r#"
interface Marker {
    val marker: Boolean get() = false
}
fun interface Action : Marker { fun run() }
class Consumer { fun accept(action: Action) { action.run() } }
fun forward(consumer: Consumer, callback: () -> Unit) {
    consumer.accept(callback)
}
"#);
}

#[test]
fn expected_member_result_constrains_the_enclosing_builder() {
    oks(r#"
class Product
class Builder<C> {
    fun yield(value: C) {}
    fun materialize(): C = Product() as C
}
fun <T> T.apply(block: T.() -> Unit): T = this
fun <F> build(block: Builder<F>.() -> Unit): Builder<F> = Builder<F>().apply(block)
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() {
    val yielded = build { yield(Product()) }
    exact<Builder<Product>>(yielded)
    fun nested() {
        fun consume(value: Product) {}
        val built = build { consume(materialize()) }
        exact<Builder<Product>>(built)
    }
    nested()
}
"#);
}

#[test]
fn postponed_receiver_lambda_result_shapes_the_next_lambda_argument() {
    ok(r#"
class Target
class Inner<T> { fun set(value: T) {} }
fun <T> inner(block: Inner<T>.() -> Unit): Inner<T> = Inner<T>()
class Outer<T>
fun <T> outer(value: T, block: Outer<T>.(T) -> Unit) {}
fun use() {
    outer(inner { set(Target()) }) { it }
}
"#);
}

#[test]
fn expected_result_flows_through_nested_generic_suspend_calls() {
    oks(r#"
suspend fun <T> produce(): T = null as T
suspend fun <T> identity(value: T): T = value
suspend fun consume(): Int = identity(produce())

class Calls {
    suspend fun <T> memberProduce(): T = null as T
    suspend fun <T> memberIdentity(value: T): T = value
    suspend fun consume(): Int = memberIdentity(memberProduce())
}

@kotlin.jvm.JvmInline value class Token(val value: Int)
class ValueCalls {
    suspend fun <T> produce(): T = null as T
    suspend fun <T> identity(value: T): T = value
    suspend fun wrap(value: Token): Token = Token(value.value)
    suspend fun <T> forward(value: T): T = value
    suspend fun consume(): Token = identity(wrap(forward(produce())))
}

@kotlin.jvm.JvmInline value class AnyToken(val value: Any)
class AnyValueCalls {
    suspend fun <T> produce(): T = null as T
    suspend fun <T> identity(value: T): T = value
    suspend fun wrap(value: AnyToken): AnyToken = AnyToken(value.value)
    suspend fun <T> forward(value: T): T = value
    suspend fun consume(): AnyToken = identity(wrap(forward(produce())))
}
"#);
}

#[test]
fn generic_local_function_transfers_postponed_lambda_constraints() {
    oks(r#"
class Buildee<T> { fun materialize(): T = Value() as T }
class Value
fun <T> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() {
    val built = build {
        fun <T> share(first: T, second: T) {}
        share(Value(), materialize())
    }
    exact<Buildee<Value>>(built)
}
"#);
}

#[test]
fn local_consumer_constrains_a_postponed_receiver_member_result() {
    oks(r#"
class Buildee<T> { fun materialize(): T = 42 as T }
fun <T> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() {
    fun consume(value: Int) {}
    val built = build { consume(materialize()) }
    exact<Buildee<Int>>(built)
}
"#);
}

#[test]
fn generic_extension_property_preserves_a_postponed_local_class_type() {
    ok(r#"
private var storage: Any? = null
class Buildee<T> { fun yield(value: T) {} }
fun <T> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()
val <T> Buildee<T>.value: T get() = storage as T
fun use() {
    val built = build {
        class Local { fun localOnly() {} }
        val value = Local()
        storage = value
        yield(value)
    }
    built.value.localOnly()
}
"#);
}

#[test]
fn unshaped_generic_lambda_has_the_calls_implicit_return_label() {
    oks(r#"
fun <T> identity(value: T): T = value
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> exact(value: @kotlin.internal.Exact T) {}
fun use() {
    val result = identity { return@identity "value" }
    exact<() -> String>(result)
}
"#);
}

#[test]
fn anonymous_object_method_captures_its_enclosing_receiver_lambda() {
    ok(r#"
class Builder<T> { fun yield(value: T) {} }
fun <T> build(block: Builder<T>.() -> Unit): Builder<T> = Builder<T>()
fun use() {
    build {
        val value = object {
            fun initialize() { yield(this) }
        }
        value.initialize()
    }
}
"#);
}

#[test]
fn exact_generic_parameters_share_one_inferred_anonymous_type() {
    oks(r#"
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
fun <T> same(first: @kotlin.internal.Exact T, second: @kotlin.internal.Exact T) {}
fun use() {
    val value = object {}
    same(value, value)
}
"#);
}

#[test]
fn expected_unit_lambda_checks_trailing_when_as_a_statement() {
    ok("fun consumeUnit(block: () -> Unit) { block() }\n\
            fun statementWhen(value: Int) {\n\
                consumeUnit { when (value) { 2 -> println(value) } }\n\
            }\n\
            fun consumeReceiver(block: String.() -> Unit) { \"value\".block() }\n\
            fun receiverStatementWhen(value: Int) {\n\
                consumeReceiver { when (value) { 2 -> println(length) } }\n\
            }");
}

#[test]
fn unused_implicit_it_adopts_the_selected_unary_function_shape() {
    ok("fun consume(block: (Int) -> Unit) {}\n\
            fun use() { consume { val ignored = 0 } }");
}

#[test]
fn explicit_empty_lambda_parameter_list_remains_zero_arity() {
    let (errors, _) = check(
        "fun consume(block: (Int) -> Unit) {}\n\
             fun use() { consume { -> val ignored = 0 } }",
    );

    assert_eq!(
            errors,
            ["argument type mismatch: actual type is '() -> Unit', but '(Int) -> Unit' was expected."]
        );
}

#[test]
fn expected_unit_member_lambda_checks_trailing_when_as_a_statement() {
    ok("class Consumer {\n\
                fun consume(block: () -> Unit) { block() }\n\
                fun consumeReceiver(block: String.() -> Unit) { \"value\".block() }\n\
                fun consumeSuspend(block: suspend () -> Unit) {}\n\
            }\n\
            fun memberStatementWhen(consumer: Consumer, value: Int) {\n\
                consumer.consume { when (value) { 2 -> println(value) } }\n\
                consumer.consumeReceiver { when (value) { 2 -> println(length) } }\n\
                consumer.consumeSuspend { when (value) { 2 -> println(value) } }\n\
            }");
}

#[test]
fn expected_unit_context_member_lambda_checks_trailing_when_as_a_statement() {
    let errors = check_with_detected_features(
        "// LANGUAGE: +ContextParameters\n\
             class Consumer {\n\
                 fun consumeContext(block: context(String) () -> Unit) {}\n\
             }\n\
             fun memberStatementWhen(consumer: Consumer, value: Int) {\n\
                 consumer.consumeContext { when (value) { 2 -> println(length) } }\n\
             }",
    );

    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn context_extension_index_checks_only_value_parameters() {
    let errors = check_with_detected_features(
        "// LANGUAGE: +ContextParameters\n\
             class Container(var value: String)\n\
             context(ctx: Int)\n\
             operator fun Container.get(index: Int): String = value\n\
             context(ctx: Int)\n\
             operator fun Container.set(index: Int, newValue: String) { value = newValue }\n\
             context(ctx: Int)\n\
             fun read(container: Container): String {\n\
                 container[0] = \"OK\"\n\
                 return container[0]\n\
             }",
    );

    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn expected_unit_safe_call_member_lambda_checks_trailing_when_as_a_statement() {
    ok("class Consumer {\n\
                fun consume(block: () -> Unit) { block() }\n\
                fun consumeReceiver(block: String.() -> Unit) { \"value\".block() }\n\
            }\n\
            fun safeMemberStatementWhen(consumer: Consumer?, value: Int) {\n\
                consumer?.consume { when (value) { 2 -> println(value) } }\n\
                consumer?.consumeReceiver { when (value) { 2 -> println(length) } }\n\
            }");
}

#[test]
fn safe_call_result_nullability_makes_member_inapplicable_before_extension_fallback() {
    ok("class Bag<T> {
                operator fun contains(value: T): Boolean = true
            }
            operator fun <T> Bag<out T>.contains(value: T): Boolean = true
            fun <R> String.mapValue(block: (String) -> R): R = block(this)
            fun acceptsNullableResult(text: String?, bag: Bag<Int>): Boolean =
                text?.mapValue { 1 } in bag");
}

/// A NESTED sealed class is exhausted by covering its own subclasses: Kotlin's rule is over the
/// leaves of the sealed tree, and a sealed class is abstract, so `Leaf` itself is never an
/// instance.
#[test]
fn nested_sealed_hierarchy_is_exhausted_by_its_leaves() {
    ok("sealed class Node {\n\
                sealed class Leaf : Node()\n\
                class IntLeaf : Leaf()\n\
                class StrLeaf : Leaf()\n\
                class Branch : Node()\n\
            }\n\
            fun count(n: Node): Int = when (n) {\n\
                is Node.IntLeaf -> 1\n\
                is Node.StrLeaf -> 1\n\
                is Node.Branch -> 2\n\
            }");
}

/// The intermediate sealed class covered DIRECTLY covers its whole branch — its children must not
/// be re-reported through it.
#[test]
fn covering_a_nested_sealed_class_directly_covers_its_branch() {
    ok("sealed class Node {\n\
                sealed class Leaf : Node()\n\
                class IntLeaf : Leaf()\n\
                class StrLeaf : Leaf()\n\
                class Branch : Node()\n\
            }\n\
            fun count(n: Node): Int = when (n) {\n\
                is Node.Leaf -> 1\n\
                is Node.Branch -> 2\n\
            }");
}

/// Expansion does not weaken the diagnostic: a genuinely uncovered LEAF is still named, by its own
/// name rather than its sealed parent's.
#[test]
fn a_missing_nested_sealed_leaf_is_reported_by_name() {
    let (errors, _) = check(
        "sealed class Node {\n\
                 sealed class Leaf : Node()\n\
                 class IntLeaf : Leaf()\n\
                 class StrLeaf : Leaf()\n\
                 class Branch : Node()\n\
             }\n\
             fun count(n: Node): Int = when (n) {\n\
                 is Node.IntLeaf -> 1\n\
                 is Node.Branch -> 2\n\
             }",
    );

    assert_eq!(
            errors,
            ["'when' expression must be exhaustive. Add the 'is StrLeaf' branch or an 'else' branch."]
        );
}

#[test]
fn subject_membership_is_a_predicate_for_an_arbitrary_contains_receiver() {
    ok(
        "operator fun Int.contains(value: Int): Boolean = value == this\n\
            fun f(value: Int): String = when (value) {\n\
                in 2 -> \"two\"\n\
                !in 3 -> \"not three\"\n\
                else -> \"three\"\n\
            }",
    );
}

#[test]
fn non_unit_lambda_still_requires_an_exhaustive_trailing_when() {
    let (errors, _) = check(
        "fun consumeInt(block: () -> Int): Int = block()\n\
             fun valueWhen(value: Int): Int =\n\
                 consumeInt { when (value) { 2 -> value } }",
    );

    assert_eq!(
        errors,
        ["'when' expression must be exhaustive. Add an 'else' branch."]
    );
}

#[test]
fn non_unit_member_lambda_still_requires_an_exhaustive_trailing_when() {
    let (errors, _) = check(
        "class Consumer {\n\
                 fun consumeInt(block: () -> Int): Int = block()\n\
             }\n\
             fun valueWhen(consumer: Consumer, value: Int): Int =\n\
                 consumer.consumeInt { when (value) { 2 -> value } }",
    );

    assert_eq!(
        errors,
        ["'when' expression must be exhaustive. Add an 'else' branch."]
    );
}

#[test]
fn non_unit_safe_call_member_lambda_still_requires_an_exhaustive_trailing_when() {
    let (errors, _) = check(
        "class Consumer {\n\
                 fun consumeInt(block: () -> Int): Int = block()\n\
             }\n\
             fun valueWhen(consumer: Consumer?, value: Int): Int? =\n\
                 consumer?.consumeInt { when (value) { 2 -> value } }",
    );

    assert_eq!(
        errors,
        ["'when' expression must be exhaustive. Add an 'else' branch."]
    );
}

#[test]
fn non_public_module_classifiers_keep_member_access() {
    ok("private class Local { fun value(): Int = 1 }\n\
             internal class Shared { fun value(): Int = 1 }\n\
             fun use(): Int = Local().value() + Shared().value()");
}

#[test]
fn conflict_display_bounds_identifiers_and_type_bounds() {
    let parameter = "p".repeat(MAX_OVERLOAD_DIAGNOSTIC_BYTES);
    let type_params = (0..4096)
        .map(|index| format!("T{index}: Any"))
        .collect::<Vec<_>>()
        .join(", ");
    for source in [
        format!("fun crowded({parameter}: Int): Int = 0"),
        format!("fun <{type_params}> crowded(): Int = 0"),
    ] {
        let mut diagnostics = DiagSink::new();
        let file = parse_file(&source, &mut diagnostics);
        assert_no_diags(&diagnostics);
        let function = file
            .decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Fun(function) => Some(function),
                _ => None,
            })
            .expect("source should contain a function");

        assert!(source_function_conflict_display(&file, function, 128).is_none());
    }
}

#[test]
fn elvis_uses_the_non_null_left_operand_type() {
    let source = "class StoredItem\n\
             fun pick(value: Any) {\n\
                 val item = value as? StoredItem ?: StoredItem()\n\
             }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let elvis = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| {
            matches!(expression, Expr::Elvis { .. }).then_some(ExprId(index as u32))
        })
        .expect("elvis expression");
    assert_eq!(info.ty(elvis), Ty::obj("StoredItem"));
}

#[test]
fn elvis_joins_non_null_and_nullable_string_to_nullable_string() {
    let source = "fun pick(name: String, fallback: String?) {\n\
                 val chosen = name ?: fallback\n\
             }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let elvis = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| {
            matches!(expression, Expr::Elvis { .. }).then_some(ExprId(index as u32))
        })
        .expect("elvis expression");
    assert_eq!(info.ty(elvis), Ty::nullable(Ty::String));
}

#[test]
fn same_type_class_join_unifies_spellings_without_inventing_nullability() {
    assert_eq!(
        same_type_class_join(Ty::String, Ty::nullable(Ty::String)),
        Some(Ty::nullable(Ty::String))
    );
    // Mixed NON-NULL spellings remain non-null. The original follow-up returned `String?`
    // here merely because the two representations were unequal.
    assert_eq!(
        same_type_class_join(Ty::String, Ty::obj("kotlin/String")),
        Some(Ty::String)
    );
    assert_eq!(
        same_type_class_join(Ty::String, Ty::nullable(Ty::obj("kotlin/String"))),
        Some(Ty::nullable(Ty::String))
    );
    assert_eq!(
        same_type_class_join(Ty::platform_nullable(Ty::String), Ty::String),
        Some(Ty::platform_nullable(Ty::String))
    );
    let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
    assert_eq!(
        same_type_class_join(parameter, Ty::nullable(parameter)),
        Some(Ty::nullable(parameter))
    );
    assert_eq!(
        same_type_class_join(Ty::obj("kotlin/String"), Ty::String),
        Some(Ty::String)
    );
    // Generic applications need classifier variance metadata, which this spelling-only helper
    // deliberately cannot guess. The checker's semantic common-supertype path owns them.
    assert_eq!(
        same_type_class_join(
            Ty::obj_args("kotlin/collections/List", &[Ty::Int]),
            Ty::obj_args("kotlin/collections/List", &[Ty::String])
        ),
        None
    );
    assert_eq!(same_type_class_join(Ty::String, Ty::Int), None);
    assert_eq!(
        same_type_class_join(Ty::String, Ty::obj("kotlin/Any")),
        None
    );
}

#[test]
fn super_calls_record_the_exact_source_overload() {
    let source = r#"
open class Base {
    open fun pick(value: Int): Int = value
    open fun pick(value: String): Int = value.length
}
class Child : Base() {
    fun pickInt(): Int = super.pick(1)
    fun pickString(): Int = super.pick("x")
}
"#;
    let (_, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let mut targets = info
        .resolved_super_calls
        .values()
        .map(|target| (target.owner.render(), target.params.clone()))
        .collect::<Vec<_>>();
    targets.sort_by_key(|(_, params)| format!("{params:?}"));
    assert_eq!(
        targets,
        vec![
            ("Base".to_string(), vec![Ty::Int]),
            ("Base".to_string(), vec![Ty::String]),
        ]
    );
}

#[test]
fn super_properties_record_exact_non_virtual_accessors() {
    let source = r#"
open class Base {
    open var value: Int = 0
}
class Child : Base() {
    fun update(): Int {
        super.value = super.value + 1
        return super.value
    }
}
"#;
    let (_, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    assert_eq!(
        info.resolved_super_calls
            .values()
            .filter(|target| target.name == "getValue")
            .count(),
        2
    );
    assert!(info.stmt_lowers.values().any(|lowering| {
        matches!(
            lowering,
            StmtLowering::SuperPropertyWrite { target }
                if target.owner.matches("Base") && target.name == "setValue"
        )
    }));
}

#[test]
fn implicit_source_extensions_preserve_lambda_literal_unit_coercion() {
    let source = "class Scope {\n\
                 fun choose(): String {\n\
                     val stored: () -> String = { \"value\" }\n\
                     return pick { \"value\" } + pick(stored)\n\
                 }\n\
             }\n\
             fun Scope.pick(block: () -> Unit): String = \"unit\"\n\
             fun Scope.pick(value: Any): String = \"any\"";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let declarations = file
            .decls
            .iter()
            .copied()
            .filter(|declaration| {
                matches!(file.decl(*declaration), Decl::Fun(function) if function.name == "pick")
            })
            .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 2);

    let mut literal_call = None;
    let mut stored_call = None;
    for (index, expression) in file.expr_arena.iter().enumerate() {
        let Expr::Call { callee, args } = expression else {
            continue;
        };
        if !matches!(file.expr(*callee), Expr::Name(name) if name == "pick") {
            continue;
        }
        let call = ExprId(index as u32);
        if args
            .first()
            .is_some_and(|argument| matches!(file.expr(*argument), Expr::Lambda { .. }))
        {
            literal_call = Some(call);
        } else {
            stored_call = Some(call);
        }
    }

    assert_eq!(
        info.resolved_source_call(literal_call.expect("literal pick call")),
        Some((0, declarations[0].0))
    );
    assert_eq!(
        info.resolved_source_call(stored_call.expect("stored pick call")),
        Some((0, declarations[1].0))
    );
}

#[test]
fn indexed_source_extensions_preserve_lambda_literal_unit_coercion() {
    let source = "class Scope\n\
             operator fun Scope.get(block: () -> Unit): String = \"unit\"\n\
             operator fun Scope.get(value: Any): String = \"any\"\n\
             fun literal(scope: Scope): String = scope[{ \"value\" }]\n\
             fun stored(scope: Scope, block: () -> String): String = scope[block]";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let declarations = file
            .decls
            .iter()
            .copied()
            .filter(|declaration| {
                matches!(file.decl(*declaration), Decl::Fun(function) if function.name == "get")
            })
            .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 2);

    let mut literal_index = None;
    let mut stored_index = None;
    for (index, expression) in file.expr_arena.iter().enumerate() {
        let Expr::Index { indices, .. } = expression else {
            continue;
        };
        let indexed = ExprId(index as u32);
        if indices
            .first()
            .is_some_and(|argument| matches!(file.expr(*argument), Expr::Lambda { .. }))
        {
            literal_index = Some(indexed);
        } else {
            stored_index = Some(indexed);
        }
    }

    assert_eq!(
        info.resolved_source_call(literal_index.expect("literal index call")),
        Some((0, declarations[0].0))
    );
    assert_eq!(
        info.resolved_source_call(stored_index.expect("stored index call")),
        Some((0, declarations[1].0))
    );
}

#[test]
fn safe_call_source_extensions_preserve_lambda_literal_unit_coercion() {
    let source = "class Scope\n\
             fun Scope.pick(block: () -> Unit): String = \"unit\"\n\
             fun Scope.pick(value: Any): String = \"any\"\n\
             fun literal(scope: Scope?): String? = scope?.pick { \"value\" }\n\
             fun stored(scope: Scope?, block: () -> String): String? = scope?.pick(block)";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let declarations = file
            .decls
            .iter()
            .copied()
            .filter(|declaration| {
                matches!(file.decl(*declaration), Decl::Fun(function) if function.name == "pick")
            })
            .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 2);

    let mut literal_call = None;
    let mut stored_call = None;
    for (index, expression) in file.expr_arena.iter().enumerate() {
        let Expr::SafeCall {
            name,
            args: Some(args),
            ..
        } = expression
        else {
            continue;
        };
        if name != "pick" {
            continue;
        }
        let call = ExprId(index as u32);
        if args
            .first()
            .is_some_and(|argument| matches!(file.expr(*argument), Expr::Lambda { .. }))
        {
            literal_call = Some(call);
        } else {
            stored_call = Some(call);
        }
    }

    assert_eq!(
        info.resolved_source_call(literal_call.expect("literal safe call")),
        Some((0, declarations[0].0))
    );
    assert_eq!(
        info.resolved_source_call(stored_call.expect("stored safe call")),
        Some((0, declarations[1].0))
    );
}

#[test]
fn binary_source_extensions_preserve_lambda_literal_unit_coercion() {
    let source = "class Scope\n\
             operator fun Scope.plus(block: () -> Unit): String = \"unit\"\n\
             operator fun Scope.plus(value: Any): String = \"any\"\n\
             fun literal(scope: Scope): String = scope + ({ \"value\" })\n\
             fun stored(scope: Scope, block: () -> String): String = scope + block";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let mut literal_binary = None;
    let mut stored_binary = None;
    for (index, expression) in file.expr_arena.iter().enumerate() {
        let Expr::Binary {
            op: BinOp::Add,
            rhs,
            ..
        } = expression
        else {
            continue;
        };
        let binary = ExprId(index as u32);
        if matches!(file.expr(*rhs), Expr::Lambda { .. }) {
            literal_binary = Some(binary);
        } else {
            stored_binary = Some(binary);
        }
    }

    let selected_params = |expression| {
        let Some(ResolvedCall::Extension(extension)) = info
            .resolved_operator_calls
            .get(&(expression, SyntheticOperatorCall::Plus))
        else {
            panic!("checker must record source plus selected for operator lowering");
        };
        extension.params.clone()
    };
    assert_eq!(
        selected_params(literal_binary.expect("literal plus expression")),
        vec![Ty::fun(Vec::new(), Ty::Unit)]
    );
    assert_eq!(
        selected_params(stored_binary.expect("stored plus expression")),
        vec![Ty::obj("kotlin/Any")]
    );
}

#[test]
fn nullable_compare_to_extensions_preserve_lambda_literal_unit_coercion() {
    let source = "operator fun Long?.compareTo(block: () -> Unit): Int = -1\n\
             operator fun Long?.compareTo(value: Any): Int = 1\n\
             fun literal(value: Long?): Boolean = value < ({ \"value\" })\n\
             fun stored(value: Long?, block: () -> String): Boolean = value < block";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let mut literal_binary = None;
    let mut stored_binary = None;
    for (index, expression) in file.expr_arena.iter().enumerate() {
        let Expr::Binary {
            op: BinOp::Lt, rhs, ..
        } = expression
        else {
            continue;
        };
        let binary = ExprId(index as u32);
        if matches!(file.expr(*rhs), Expr::Lambda { .. }) {
            literal_binary = Some(binary);
        } else {
            stored_binary = Some(binary);
        }
    }

    let selected_params = |expression| {
        let Some(ResolvedCall::Extension(extension)) = info
            .resolved_operator_calls
            .get(&(expression, SyntheticOperatorCall::CompareTo))
        else {
            panic!("checker must record source compareTo selected for operator lowering");
        };
        extension.params.clone()
    };
    assert_eq!(
        selected_params(literal_binary.expect("literal comparison")),
        vec![Ty::fun(Vec::new(), Ty::Unit)]
    );
    assert_eq!(
        selected_params(stored_binary.expect("stored comparison")),
        vec![Ty::obj("kotlin/Any")]
    );
}

#[test]
fn super_call_finds_an_inherited_overload_behind_unrelated_overloads() {
    let source = r#"
open class Base {
    open fun pick(value: Int): Int = value
}
open class Middle : Base() {
    fun pick(left: String, right: String): Int = left.length + right.length
    fun pick(left: Boolean, right: Boolean): Int = if (left == right) 1 else 0
}
class Child : Middle() {
    fun parent(): Int = super.pick(1)
}
"#;
    let (_, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );
    assert_eq!(info.resolved_super_calls.len(), 1);
    let target = info
        .resolved_super_calls
        .values()
        .next()
        .expect("checker must record the super-call target");
    assert_eq!(target.owner.render(), "Middle");
    assert_eq!(target.params, vec![Ty::Int]);
}

#[test]
fn super_call_selects_an_inherited_overload_over_a_nearer_namesake() {
    let source = r#"
open class Grand {
    open fun pick(value: Int): Int = value
}
open class Base : Grand() {
    open fun pick(value: String): Int = value.length
}
class Child : Base() {
    fun parent(): Int = super.pick(1)
}
"#;
    let (_, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );
    assert_eq!(info.resolved_super_calls.len(), 1);
    let target = info
        .resolved_super_calls
        .values()
        .next()
        .expect("checker must record the super-call target");
    assert_eq!(target.owner.render(), "Base");
    assert_eq!(target.params, vec![Ty::Int]);
}

#[test]
fn local_function_calls_record_resolved_target_for_lowering() {
    let source = r#"
fun box(): String {
    val base = 40
    fun f(x: Int = 2) = base + x
    return f().toString()
}
"#;
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let call = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Name(name) if name == "f" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain f() local function call");
    let target_stmt = file
        .stmt_arena
        .iter()
        .enumerate()
        .find_map(|(idx, stmt)| match stmt {
            Stmt::LocalFun(f) if f.name == "f" => Some(StmtId(idx as u32)),
            _ => None,
        })
        .expect("source should contain f local function declaration");

    let Some(ResolvedCall::LocalFunction(target)) = info.resolved_calls.get(&call) else {
        panic!("checker must record the selected local function target for lowering");
    };
    assert_eq!(target.stmt_id, target_stmt);
    assert_eq!(target.sig.params, vec![Ty::Int]);
    assert_eq!(target.sig.ret, Ty::Int);
    assert_eq!(target.provided_arg_count, 0);
    assert!(target.context_args.is_empty());
    assert!(
        !matches!(
            info.expr_lowers.get(&call),
            Some(ExprLowering::LocalFunction { .. })
        ),
        "ordinary local function calls must not duplicate the target in expr_lowers"
    );
}

#[test]
fn local_function_calls_record_shadowed_declaration_target() {
    let source = r#"
fun box(): String {
    fun f(x: Int = 1) = x
    fun g(): Int {
        fun f(x: Int = 2) = x + 10
        return f()
    }
    return g().toString()
}
"#;
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let inner_f_stmt = file
        .stmt_arena
        .iter()
        .enumerate()
        .filter_map(|(idx, stmt)| match stmt {
            Stmt::LocalFun(f) if f.name == "f" => Some(StmtId(idx as u32)),
            _ => None,
        })
        .next_back()
        .expect("source should contain inner f local function declaration");
    let f_call = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Name(name) if name == "f" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain f() local function call");

    let Some(ResolvedCall::LocalFunction(target)) = info.resolved_calls.get(&f_call) else {
        panic!("checker must record the selected local function target for lowering");
    };
    assert_eq!(target.stmt_id, inner_f_stmt);
    assert_eq!(target.provided_arg_count, 0);
}

#[test]
fn computed_property_getter_cycle_reports_inference_error() {
    let (errs, _) = check(
        r#"
val x get() = y
val y get() = x
"#,
    );
    assert_eq!(errs.len(), 2);
    assert_eq!(errs, vec![RECURSIVE_INFERENCE_MESSAGE; 2]);
}

#[test]
fn computed_property_retry_does_not_change_eager_initializer_order() {
    // Only executable getter bodies participate in the module-wide retry. An eager initializer
    // still observes declaration order, so the retry cannot silently widen the feature into a
    // different initialization model.
    let (errs, _) = check(
        r#"
val eager = later
val later = 1
"#,
    );
    assert_eq!(errs.len(), 1);
    assert_eq!(errs, ["variable 'later' must be initialized."]);
}

#[test]
fn local_value_root_shadows_classifier_member_paths() {
    let source = r#"
enum class Kind { PENDING }
class Holder(val PENDING: String)
fun box(): String {
    val Kind = Holder("OK")
    return Kind.PENDING
}
"#;
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );
    let member = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Member { name, .. } if name == "PENDING" => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain Kind.PENDING member read");
    assert_eq!(info.ty(member), Ty::String);
    assert!(
        info.resolved_enum_entry(member).is_none(),
        "local value root must not be recorded as an enum-entry classifier path"
    );
}

#[test]
fn non_callable_local_does_not_hide_a_constructor_from_call_syntax() {
    let (errs, _) = check(
        r#"
class Foo
fun box(): String {
    val Foo = 1
    Foo()
    return "OK"
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected Foo() to construct the class: {errs:?}"
    );
}

fn parse_file(src: &str, d: &mut DiagSink) -> File {
    let toks = lex(src, d);
    parse(src, &toks, d)
}

fn assert_no_diags(d: &DiagSink) {
    assert!(
        d.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
    );
}

fn named_call(file: &File, name: &str) -> ExprId {
    file.expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Name(callee_name) if callee_name == name => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("source should contain {name}() call"))
}

fn module_top_level_target(info: &TypeInfo, call: ExprId) -> &ResolvedTopLevelCall {
    let Some(ResolvedCall::TopLevel(target)) = info.resolved_calls.get(&call) else {
        panic!("checker must record the selected same-module top-level target for lowering");
    };
    target
}

struct FakeMemberPlatform;

impl FakeMemberPlatform {
    fn callable_symbols(&self, name: &str) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        if matches!(
            name,
            "BoxedComparable"
                | "BoxedIndex"
                | "BoxedIterable"
                | "BoxedIterator"
                | "TestMutex"
                | "Factory"
                | "JavaState"
        ) {
            return std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                builtin_classifier: false,
                classifier_name: None,
                classifier_declaration: None,
                classifier: None,
                callables: crate::libraries::Callables::None,
                importable_declaration: false,
            });
        }
        if matches!(name, "conflictingNames" | "conflictingNamesExt") {
            let extension = name.ends_with("Ext");
            let make = |params: Vec<Ty>, names: [&str; 2], descriptor: &'static str| {
                let mut physical = params.clone();
                if extension {
                    physical.insert(0, Ty::String);
                }
                let mut info = crate::libraries::FunctionInfo::plain(
                    if extension {
                        crate::libraries::FnKind::Extension
                    } else {
                        crate::libraries::FnKind::TopLevel
                    },
                    extension.then_some(Ty::String),
                    crate::libraries::LibraryCallable::library(
                        "test/ConflictingKt",
                        if extension {
                            "conflictingNamesExt"
                        } else {
                            "conflictingNames"
                        },
                        physical,
                        Ty::Int,
                        Ty::Int,
                        descriptor,
                    ),
                );
                info.call_sig = CallSig {
                    param_names: names.into_iter().map(str::to_string).collect(),
                    required: 2,
                    ..Default::default()
                };
                info
            };
            return std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                builtin_classifier: false,
                classifier_name: None,
                classifier_declaration: None,
                classifier: None,
                callables: crate::libraries::Callables::Functions(crate::libraries::FunctionSet {
                    overloads: vec![
                        make(
                            vec![Ty::Int, Ty::Int],
                            ["a", "c"],
                            if extension {
                                "(Ljava/lang/String;II)I"
                            } else {
                                "(II)I"
                            },
                        ),
                        make(
                            vec![Ty::String, Ty::String],
                            ["b", "d"],
                            if extension {
                                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I"
                            } else {
                                "(Ljava/lang/String;Ljava/lang/String;)I"
                            },
                        ),
                    ],
                }),
                importable_declaration: false,
            });
        }
        let (kind, receiver, owner, name, params, ret, descriptor) = match name {
            "knownTop" => (
                crate::libraries::FnKind::TopLevel,
                None,
                "test/TopKt",
                "knownTop",
                vec![Ty::String, Ty::Int],
                Ty::String,
                "(Ljava/lang/String;I)Ljava/lang/String;",
            ),
            "knownPair" => (
                crate::libraries::FnKind::TopLevel,
                None,
                "test/TopKt",
                "knownPair",
                vec![Ty::Int, Ty::Int],
                Ty::Int,
                "(II)I",
            ),
            "mixExt" => (
                crate::libraries::FnKind::Extension,
                Some(Ty::String),
                "test/TextKt",
                "mixExt",
                vec![Ty::String, Ty::Int, Ty::Int],
                Ty::Int,
                "(Ljava/lang/String;II)I",
            ),
            "typedExt" => (
                crate::libraries::FnKind::Extension,
                Some(Ty::String),
                "test/TextKt",
                "typedExt",
                vec![Ty::String, Ty::String, Ty::Int],
                Ty::Int,
                "(Ljava/lang/String;Ljava/lang/String;I)I",
            ),
            "literalChoice" => (
                crate::libraries::FnKind::Extension,
                Some(Ty::String),
                "test/TextKt",
                "literalChoice",
                vec![Ty::String, Ty::fun(Vec::new(), Ty::Unit)],
                Ty::String,
                "(Ljava/lang/String;Lkotlin/jvm/functions/Function0;)Ljava/lang/String;",
            ),
            "Foo" => (
                crate::libraries::FnKind::TopLevel,
                None,
                "test/TopKt",
                "Foo",
                vec![Ty::String],
                Ty::String,
                "(Ljava/lang/String;)Ljava/lang/String;",
            ),
            "getValue" => (
                crate::libraries::FnKind::Extension,
                Some(Ty::obj("test/Delegate")),
                "test/DelegateKt",
                "getValue",
                vec![
                    Ty::obj("test/Delegate"),
                    Ty::nullable(Ty::obj("kotlin/Any")),
                    Ty::obj("kotlin/reflect/KProperty"),
                ],
                Ty::String,
                "(LDelegate;Ljava/lang/Object;Lkotlin/reflect/KProperty;)Ljava/lang/String;",
            ),
            "decorate" => (
                crate::libraries::FnKind::Extension,
                Some(Ty::String),
                "test/TextKt",
                "decorate",
                vec![Ty::String],
                Ty::String,
                "(Ljava/lang/String;)Ljava/lang/String;",
            ),
            "tag" => (
                crate::libraries::FnKind::Extension,
                Some(Ty::String),
                "test/TextKt",
                "tag",
                vec![Ty::String, Ty::Int],
                Ty::String,
                "(Ljava/lang/String;I)Ljava/lang/String;",
            ),
            "containerTag" => (
                crate::libraries::FnKind::Extension,
                Some(Ty::obj("Container")),
                "test/ContainerKt",
                "containerTag",
                vec![Ty::obj("Container")],
                Ty::String,
                "(LContainer;)Ljava/lang/String;",
            ),
            _ => return std::rc::Rc::new(crate::libraries::ResolvedSymbols::default()),
        };
        let mut callable =
            crate::libraries::LibraryCallable::library(owner, name, params, ret, ret, descriptor);
        callable.external_identity = Some(crate::fir::ExternalCallableId::from_raw(match name {
            "knownTop" => 10,
            "knownPair" => 11,
            "mixExt" => 12,
            "typedExt" => 13,
            "literalChoice" => 14,
            "Foo" => 15,
            "getValue" => 16,
            "decorate" => 17,
            "tag" => 18,
            "containerTag" => 19,
            _ => unreachable!("the fake platform filters unknown callable names above"),
        }));
        let mut info = crate::libraries::FunctionInfo::plain(kind, receiver, callable);
        if name == "getValue" {
            // Delegated-property conventions are selected by declaration metadata, never by
            // spelling. This fake classpath record must model kotlinc's `operator` flag just as
            // the real Kotlin metadata provider does.
            info.flags.operator = true;
        }
        info.call_sig = CallSig {
            param_names: if matches!(name, "knownTop" | "knownPair" | "mixExt" | "typedExt") {
                vec!["a".to_string(), "b".to_string()]
            } else if name == "getValue" {
                vec!["thisRef".to_string(), "property".to_string()]
            } else if matches!(name, "tag" | "literalChoice") {
                vec!["a".to_string()]
            } else {
                vec!["s".to_string()]
            },
            param_defaults: Vec::new().into(),
            lambda_param_types: Vec::new(),
            required: match name {
                "knownTop" | "knownPair" | "mixExt" | "typedExt" | "getValue" => 2,
                _ => 1,
            },
            ..Default::default()
        };
        std::rc::Rc::new(crate::libraries::ResolvedSymbols {
            builtin_classifier: false,
            classifier_name: None,
            classifier_declaration: None,
            classifier: None,
            callables: crate::libraries::Callables::Functions(crate::libraries::FunctionSet {
                overloads: vec![info],
            }),
            importable_declaration: false,
        })
    }

    fn classifier_record(
        &self,
        internal: TypeName,
    ) -> Option<std::sync::Arc<crate::libraries::LibraryType>> {
        if internal.matches("test/Foo") || internal.matches("test/FooDerived") {
            let foo = crate::types::type_name("test/Foo");
            let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
            let (supertypes, supertype_templates, declared_callables, type_parameters) = if internal
                == foo
            {
                let mut getter = crate::libraries::LibraryCallable::library(
                    foo,
                    "getProp",
                    vec![parameter],
                    Ty::String,
                    Ty::String,
                    "(Ljava/lang/Object;)Ljava/lang/String;",
                );
                getter.external_identity = Some(crate::fir::ExternalCallableId::from_raw(1));
                getter.external_property_identity =
                    Some(crate::fir::ExternalPropertyId::from_raw(1));
                let property = crate::libraries::PropertyInfo {
                    return_value_status: None,
                    name: "prop".to_string(),
                    kind: crate::libraries::PropKind::MemberExtension,
                    receiver: Some(parameter),
                    associated_classifier: None,
                    associated_access_owner: None,
                    companion_associated: false,
                    formals: Vec::new(),
                    ty: Ty::String,
                    context_count: 0,
                    context_param_names: Vec::new(),
                    context_parameter_identities: Vec::new(),
                    getter,
                    setter: None,
                    setter_visibility: Visibility::Private,
                    setter_parameter_name: None,
                    is_const: false,
                    implicit_integer_coercion: false,
                    compile_time_constant: None,
                    metadata_constant_read: false,
                    deprecated_hidden: false,
                    accessor_availability: Default::default(),
                    is_final: false,
                    visibility: Visibility::Public,
                    owner: foo,
                    receiver_rank: 0,
                    source_key: None,
                    stable_declaration: None,
                    source_visible: true,
                    getter_declaration: None,
                    setter_declaration: None,
                    source_member: None,
                    producer: crate::libraries::PropertyProducer::KotlinAccessor,
                    read_stability: crate::libraries::PropertyReadStability::Unstable,
                };
                (
                    crate::types::TypeNameList::new(),
                    Vec::new(),
                    HashMap::from_iter([(
                        "prop".to_string(),
                        crate::libraries::Callables::Properties(crate::libraries::PropertySet {
                            overloads: vec![property],
                        }),
                    )]),
                    crate::types::TypeParameters::new(
                        vec!["T".to_string()],
                        vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
                        vec![crate::types::TypeVariance::Invariant],
                    ),
                )
            } else {
                (
                    vec![foo].into(),
                    vec![Ty::obj_args_name(foo, &[Ty::String])],
                    HashMap::default(),
                    crate::types::TypeParameters::default(),
                )
            };
            return Some(std::sync::Arc::new(crate::libraries::LibraryType {
                klib_identity: None,
                runtime_exception: None,
                declaration_name: None,
                is_kotlin: true,
                is_data: false,
                access: crate::libraries::ClassifierAccess::Public,
                source_file: None,
                stable_declaration: None,
                is_nested: false,
                outer_instance: None,
                kind: crate::libraries::TypeKind::Interface,
                inheritance: Default::default(),
                supertypes,
                supertype_templates,
                constructors: vec![],
                hidden_member_properties: Default::default(),
                hidden_deprecated_callables: Default::default(),
                declared_callables,
                declared_callable_order: if internal == foo {
                    vec!["prop".to_string()]
                } else {
                    Default::default()
                },
                members: vec![],
                methods_as_declarations: false,
                companion: vec![],
                constants: HashMap::default(),
                sam_eligible: false,
                callable_signature: None,
                callable_signatures: Vec::new(),
                companion_object: None,
                qualified_name: None,
                value_underlying: None,
                value_declaration: None,
                alias_target: None,
                own_type_parameter_count: type_parameters.type_params.len(),
                type_parameters,
                sealed_subclasses: crate::types::TypeNameList::new(),
                enum_entries: Vec::new(),
                enum_entries_accessor: None,
                named_parameter_lists: Vec::new(),
                annotations: Vec::new(),
                retention: None,
                annotation_targets: None,
                mapped_collection: None,
                mapped_builtin: false,
                annotation_element_defaults: Vec::new(),
                deprecation: None,
            }));
        }
        [
            "kotlin/String",
            "BoxedComparable",
            "BoxedIndex",
            "BoxedIterable",
            "BoxedIterator",
            "TestMutex",
            "test/Factory",
            "test/JavaState",
        ]
        .iter()
        .any(|name| internal.matches(name))
        .then(|| {
            let companion = if internal.matches("test/Factory") {
                let member = |second, descriptor: &str| {
                    let mut member = crate::libraries::LibraryMember::new(
                        "make".to_string(),
                        vec![Ty::obj("Config"), second],
                        Ty::String,
                        descriptor.to_string(),
                    );
                    member.owner = Some(crate::types::type_name("test/Factory"));
                    member.physical_parameter_plan = None;
                    member.call_sig = CallSig::default();
                    member
                };
                let mut companion = vec![
                    member(
                        Ty::obj("kotlin/Any"),
                        "(LConfig;Ljava/lang/Object;)Ljava/lang/String;",
                    ),
                    member(Ty::obj("Handler"), "(LConfig;LHandler;)Ljava/lang/String;"),
                ];
                let mut exists = crate::libraries::LibraryMember::new(
                    "exists".to_string(),
                    vec![Ty::obj("Config"), Ty::array(Ty::obj("kotlin/Any"))],
                    Ty::Boolean,
                    "(LConfig;[Ljava/lang/Object;)Z".to_string(),
                );
                exists.owner = Some(crate::types::type_name("test/Factory"));
                exists.external_identity = Some(crate::fir::ExternalCallableId::from_raw(99));
                exists.declaration_owner = Some(crate::types::SemanticCallableOwner::Classifier(
                    crate::types::type_name("test/Factory"),
                ));
                exists.call_sig = CallSig {
                    required: 1,
                    vararg: true,
                    vararg_index: Some(1),
                    ..CallSig::default()
                };
                companion.push(exists);
                companion
            } else {
                Vec::new()
            };
            let receiver = if internal.matches("kotlin/String") {
                Ty::String
            } else {
                Ty::obj_name(internal)
            };
            let names: &[&str] = if internal.matches("TestMutex") {
                &["lock", "unlock"]
            } else if internal.matches("BoxedComparable") {
                &["compareTo"]
            } else if internal.matches("BoxedIndex") {
                &["get", "set"]
            } else if internal.matches("BoxedIterable") {
                &["iterator"]
            } else if internal.matches("BoxedIterator") {
                &["hasNext", "next"]
            } else if internal.matches("kotlin/String") {
                &[
                    "get",
                    "known",
                    "typedKnown",
                    "choose",
                    "tie",
                    "literalChoice",
                ]
            } else {
                &[]
            };
            let declared_callables = names
                .iter()
                .filter_map(|name| {
                    let declarations = self.declared_callables_for(receiver, name);
                    (!matches!(declarations, crate::libraries::Callables::None))
                        .then(|| ((*name).to_string(), declarations))
                })
                .collect();
            std::sync::Arc::new(crate::libraries::LibraryType {
                klib_identity: None,
                runtime_exception: None,
                declaration_name: None,
                is_kotlin: true,
                is_data: false,
                access: crate::libraries::ClassifierAccess::Public,
                source_file: None,
                stable_declaration: None,
                is_nested: false,
                outer_instance: None,
                kind: if internal.matches("test/JavaState") {
                    crate::libraries::TypeKind::Enum
                } else {
                    crate::libraries::TypeKind::Class
                },
                inheritance: Default::default(),
                supertypes: crate::types::TypeNameList::new(),
                supertype_templates: Vec::new(),
                constructors: vec![],
                hidden_member_properties: Default::default(),
                hidden_deprecated_callables: Default::default(),
                declared_callables,
                declared_callable_order: names.iter().map(|name| (*name).to_string()).collect(),
                members: vec![],
                methods_as_declarations: false,
                companion,
                constants: HashMap::default(),
                sam_eligible: false,
                callable_signature: None,
                callable_signatures: Vec::new(),
                companion_object: None,
                qualified_name: None,
                value_underlying: None,
                value_declaration: None,
                alias_target: None,
                type_parameters: crate::types::TypeParameters::default(),
                own_type_parameter_count: 0,
                sealed_subclasses: crate::types::TypeNameList::new(),
                enum_entries: if internal.matches("test/JavaState") {
                    vec!["READY".to_string()]
                } else {
                    Vec::new()
                },
                enum_entries_accessor: None,
                named_parameter_lists: vec![],
                annotations: Vec::new(),
                retention: None,
                annotation_targets: None,
                mapped_collection: None,
                mapped_builtin: false,
                annotation_element_defaults: Vec::new(),
                deprecation: None,
            })
        })
    }
}

impl crate::symbol_source::SymbolSource for FakeMemberPlatform {
    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        parent == TypeName::ROOT && name == "test"
    }

    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        let mut record = (*self.callable_symbols(name)).clone();
        let core = crate::libraries::EmptySymbolSource.symbols(namespace, name);
        let internal = match namespace {
            crate::symbol_source::SymbolNamespace::Package(package)
                if package == TypeName::ROOT
                    && matches!(
                        name,
                        "BoxedComparable"
                            | "BoxedIndex"
                            | "BoxedIterable"
                            | "BoxedIterator"
                            | "TestMutex"
                    ) =>
            {
                Some(crate::types::type_name_child(package, name))
            }
            crate::symbol_source::SymbolNamespace::Package(package)
                if package.matches("kotlin") && name == "String" =>
            {
                Some(crate::types::type_name_child(package, name))
            }
            crate::symbol_source::SymbolNamespace::Package(package)
                if package.matches("test")
                    && matches!(name, "Factory" | "JavaState" | "Foo" | "FooDerived") =>
            {
                Some(crate::types::type_name_child(package, name))
            }
            _ => None,
        };
        record.classifier = internal
            .and_then(|internal| self.classifier_record(internal))
            .or_else(|| core.classifier.clone());
        record.classifier_name = record
            .classifier
            .as_ref()
            .and(internal.or(core.classifier_name));
        record.classifier_declaration = record.classifier.as_ref().and_then(|_| {
            internal
                .map(crate::libraries::ClassifierDeclaration::Ordinary)
                .or_else(|| core.classifier_declaration.clone())
        });
        record.builtin_classifier = internal.is_none() && core.builtin_classifier;
        std::rc::Rc::new(record)
    }
}

impl FakeMemberPlatform {
    fn declared_callables_for(&self, recv: Ty, name: &str) -> crate::libraries::Callables {
        let functions = (|| {
            if recv == Ty::obj("TestMutex") && matches!(name, "lock" | "unlock") {
                let callable = crate::libraries::LibraryCallable::library(
                    "TestMutex",
                    name,
                    vec![Ty::obj("kotlin/Any")],
                    Ty::Unit,
                    Ty::Unit,
                    "(Ljava/lang/Object;)V",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::obj("TestMutex")),
                    callable,
                );
                info.flags.suspend = name == "lock";
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv == Ty::obj("BoxedComparable") && name == "compareTo" {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedComparable",
                    "compareTo",
                    vec![Ty::obj("BoxedComparable")],
                    Ty::Int,
                    Ty::Int,
                    "(LBoxedComparable;)I",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::obj("BoxedComparable")),
                    callable,
                );
                info.flags.operator = true;
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv == Ty::obj("BoxedIndex") && name == "get" {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIndex",
                    "get",
                    vec![Ty::Int],
                    Ty::String,
                    Ty::String,
                    "(I)Ljava/lang/String;",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::obj("BoxedIndex")),
                    callable,
                );
                info.flags.operator = true;
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv == Ty::obj("BoxedIndex") && name == "set" {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIndex",
                    "set",
                    vec![Ty::Int, Ty::String],
                    Ty::Unit,
                    Ty::Unit,
                    "(ILjava/lang/String;)V",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::obj("BoxedIndex")),
                    callable,
                );
                info.flags.operator = true;
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv == Ty::obj("BoxedIterable") && name == "iterator" {
                let iter_ty = Ty::obj_args("BoxedIterator", &[Ty::String]);
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIterable",
                    "iterator",
                    vec![],
                    iter_ty,
                    iter_ty,
                    "()LBoxedIterator;",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::obj("BoxedIterable")),
                    callable,
                );
                info.flags.operator = true;
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv
                .obj_internal()
                .is_some_and(|n| n.matches("BoxedIterator"))
                && name == "hasNext"
            {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIterator",
                    "hasNext",
                    vec![],
                    Ty::Boolean,
                    Ty::Boolean,
                    "()Z",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(recv),
                    callable,
                );
                info.flags.operator = true;
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv
                .obj_internal()
                .is_some_and(|n| n.matches("BoxedIterator"))
                && name == "next"
            {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIterator",
                    "next",
                    vec![],
                    Ty::String,
                    Ty::String,
                    "()Ljava/lang/String;",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(recv),
                    callable,
                );
                info.flags.operator = true;
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv == Ty::String && name == "get" {
                let callable = crate::libraries::LibraryCallable::library(
                    "kotlin/String",
                    "get",
                    vec![Ty::Int],
                    Ty::Char,
                    Ty::Char,
                    "(I)C",
                );
                // `kotlin.String.get` is declared `operator`, which is what admits `s[0]`.
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::String),
                    callable,
                );
                info.flags.operator = true;
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv != Ty::String
                || !matches!(
                    name,
                    "known" | "typedKnown" | "choose" | "tie" | "literalChoice"
                )
            {
                return crate::libraries::FunctionSet::default();
            }
            let member = |params: Vec<Ty>, descriptor: &'static str, defaults: Vec<bool>| {
                let has_defaults = defaults.iter().any(|default| *default);
                let callable = crate::libraries::LibraryCallable::library(
                    "test/Host",
                    name,
                    params,
                    Ty::String,
                    Ty::String,
                    descriptor,
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::String),
                    callable,
                );
                info.call_sig = CallSig {
                    param_names: if defaults.len() == 1 {
                        vec!["a".to_string()]
                    } else {
                        vec!["a".to_string(), "b".to_string()]
                    },
                    param_defaults: defaults.into(),
                    required: 1,
                    ..Default::default()
                };
                if has_defaults {
                    info.callable.default_realization =
                        Some(Box::new(crate::libraries::DefaultCallRealization {
                            owner: crate::types::type_name("test/Host"),
                            name: format!("{name}$default"),
                            descriptor:
                                "(Ljava/lang/String;IIILjava/lang/Object;)Ljava/lang/String;"
                                    .to_string(),
                            declaration_owner: crate::types::type_name("test/Host"),
                            real_params: vec![Ty::Int, Ty::Int],
                            mask_count: 1,
                            ret: Ty::String,
                            suspend: false,
                        }));
                }
                info
            };
            let overloads = match name {
                "known" => vec![member(
                    vec![Ty::Int, Ty::Int],
                    "(II)Ljava/lang/String;",
                    vec![false, true],
                )],
                "typedKnown" => vec![member(
                    vec![Ty::String, Ty::Int],
                    "(Ljava/lang/String;I)Ljava/lang/String;",
                    vec![false, false],
                )],
                "choose" => vec![
                    member(vec![Ty::Int], "(I)Ljava/lang/String;", vec![false]),
                    member(vec![Ty::Boolean], "(Z)Ljava/lang/String;", vec![false]),
                ],
                "tie" => vec![
                    member(vec![Ty::Long], "(J)Ljava/lang/String;", vec![false]),
                    member(vec![Ty::Byte], "(B)Ljava/lang/String;", vec![false]),
                ],
                "literalChoice" => {
                    vec![member(vec![Ty::Int], "(I)Ljava/lang/String;", vec![false])]
                }
                _ => Vec::new(),
            };
            crate::libraries::FunctionSet { overloads }
        })();
        crate::libraries::Callables::from_parts(functions, crate::libraries::PropertySet::default())
    }
}

impl SemanticPlatform for FakeMemberPlatform {}

#[test]
fn member_extension_preserves_both_implicit_receivers() {
    let source = "import test.containerTag\n\
             import test.decorate\n\
             object Container {\n\
                 private fun String.render() = decorate() + containerTag()\n\
             }";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    let extension_receiver_call = named_call(&file, "decorate");
    let dispatch_receiver_call = named_call(&file, "containerTag");
    assert_no_diags(&diagnostics);

    let extension = info
        .resolved_extension(extension_receiver_call)
        .expect("imported extension must resolve on the member extension receiver");
    assert_eq!(extension.owner, "test/TextKt");
    assert_eq!(extension.name, "decorate");

    let dispatch_extension = info
        .resolved_extension(dispatch_receiver_call)
        .expect("containing object must remain an implicit dispatch receiver");
    assert_eq!(dispatch_extension.owner, "test/ContainerKt");
    assert_eq!(dispatch_extension.name, "containerTag");
}

#[test]
fn inherited_classpath_member_extension_property_keeps_applied_dispatch_type() {
    let source = "import test.FooDerived\n\
             class Unspecialized : FooDerived {\n\
                 fun read(): String = \"K\".prop\n\
             }";
    let (_, info, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_no_diags(&diagnostics);

    let selected = info
        .expr_lowers
        .values()
        .find_map(|lowering| match lowering {
            ExprLowering::MemberExtensionPropertyRead {
                owner,
                receiver,
                ty,
                getter,
                ..
            } => Some((*owner, *receiver, *ty, getter.as_deref())),
            _ => None,
        });
    let (owner, receiver, ty, getter) =
        selected.expect("inherited dependency member-extension property must be selected");
    assert!(owner.matches("test/Foo"));
    assert_eq!(receiver, Ty::String);
    assert_eq!(ty, Ty::String);
    assert!(getter.is_some_and(|getter| getter.external_identity.is_some()));
}

#[test]
fn inherited_member_extension_property_override_wins_same_depth_diamond() {
    let mut diagnostics = DiagSink::new();
    let source = "interface Foo<T> {\n\
                 val T.prop: String get() = \"fail\"\n\
             }\n\
             interface Foo2 : Foo<String> {\n\
                 override val String.prop: String get() = this\n\
             }\n\
             interface Foo3 : Foo<String>, Foo2\n\
             class Base : Foo3 {\n\
                 fun read(): String = \"OK\".prop\n\
             }";
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source).with_file_stem("Diamond")],
        jvm_platform(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let info = analysis.types[0]
        .as_ref()
        .expect("production Pass 2 must check the source file");

    let selected_owner = info
        .expr_lowers
        .values()
        .find_map(|lowering| match lowering {
            ExprLowering::MemberExtensionPropertyRead { owner, .. } => Some(*owner),
            _ => None,
        });
    assert!(
        selected_owner.is_some_and(|owner| owner.matches("Foo2")),
        "the overriding declaration must win the equal-depth diamond: {selected_owner:?}"
    );
}

#[test]
fn qualified_inner_function_type_applies_explicit_outer_arguments() {
    let source = "// LANGUAGE: +ProperSupportOfInnerClassesInCallableReferenceLHS\n\
             class Outer<A> {\n\
                 inner class Inner<C> { fun <T> id(value: T): T = value }\n\
             }\n\
             fun use() {\n\
                 val reference: Outer<Int>.Inner<Int>.(String) -> String =\n\
                     Outer<Int>.Inner<Int>::id\n\
                 reference(Outer<Int>().Inner(), \"OK\")\n\
             }";
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let platform = initialized_jvm_libraries(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(classpath),
    ));
    let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(platform));
    assert_no_diags(&diagnostics);
}

#[test]
fn inner_class_literal_lhs_applies_each_enclosing_classifier_argument() {
    let source = "// LANGUAGE: +ProperSupportOfInnerClassesInCallableReferenceLHS\n\
             class ClassReference<A> {\n\
                 inner class A<K> {\n\
                     inner class DeepInner\n\
                     val refFoo = A<K>::DeepInner::class\n\
                 }\n\
             }\n\
             fun use() {\n\
                 val refBar = ClassReference<Int>.A<String>::DeepInner::class\n\
                 val raw = ClassReference.A.DeepInner::class\n\
                 val refFoo = ClassReference<Int>().A<String>().refFoo\n\
             }";
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let platform = initialized_jvm_libraries(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(classpath),
    ));
    let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(platform));
    assert_no_diags(&diagnostics);
}

#[test]
fn member_extension_prefers_extension_receiver_members() {
    let source = "class Receiver(val marker: Int) {\n\
                 fun choose(): Int = marker\n\
             }\n\
             class Container(val marker: String, val dispatchOnly: String) {\n\
                 fun choose(): String = marker\n\
                 private fun Receiver.render(): Int = marker + choose()\n\
                 private fun Receiver.dispatchRead(): String = dispatchOnly\n\
             }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    let marker = file
        .expr_arena
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, expr)| {
            matches!(expr, Expr::Name(name) if name == "marker").then_some(ExprId(idx as u32))
        })
        .expect("source should contain the member-extension property read");
    let dispatch_only = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| {
            matches!(expr, Expr::Name(name) if name == "dispatchOnly").then_some(ExprId(idx as u32))
        })
        .expect("source should contain the dispatch-receiver property read");
    let choose = named_call(&file, "choose");
    assert_no_diags(&diagnostics);

    assert_eq!(info.ty(marker), Ty::Int);
    assert_eq!(info.ty(dispatch_only), Ty::String);
    assert!(matches!(
        info.resolved_calls.get(&choose),
        Some(ResolvedCall::Member(member))
            if matches!(member.origin, Origin::Module { .. })
                && member.receiver == Ty::obj("Receiver")
    ));
}

#[test]
fn member_extension_precedes_top_level_extension() {
    assert!(check(
        "fun String.pick(): Int = 1\n\
             class Container {\n\
                 private fun String.pick(): String = this\n\
                 fun selected(): String = \"member\".pick()\n\
             }"
    )
    .0
    .is_empty());
}

#[test]
fn local_extension_precedes_top_level_extension() {
    assert!(check(
        "fun String.pick(): Int = 1\n\
             fun selected(): String {\n\
                 fun String.pick(): String = this\n\
                 return \"local\".pick()\n\
             }"
    )
    .0
    .is_empty());
}

#[test]
fn safe_call_local_extension_precedes_top_level_extension() {
    assert!(check(
        "fun String.pick(): Int = 1\n\
             fun selected(value: String?): String? {\n\
                 fun String.pick(): String = this\n\
                 return value?.pick()\n\
             }"
    )
    .0
    .is_empty());
}

#[test]
fn safe_call_member_extension_precedes_top_level_extension() {
    assert!(check(
        "fun String.pick(): Int = 1\n\
             class Container {\n\
                 private fun String.pick(): String = this\n\
                 fun selected(value: String?): String? = value?.pick()\n\
             }"
    )
    .0
    .is_empty());
}

#[test]
fn java_static_accepts_anonymous_source_subclass_argument() {
    let source = "import test.Factory\n\
             open class Handler\n\
             class Config\n\
             fun create(config: Config): String =\n\
                 Factory.make(config, object : Handler() {})";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_no_diags(&diagnostics);

    let call = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Member { name, .. } if name == "make" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain the static factory call");
    let Some(ResolvedCall::Companion(selected)) = info.resolved_calls.get(&call) else {
        panic!("checker must record the selected Java static overload");
    };
    assert_eq!(selected.params, vec![Ty::obj("Config"), Ty::obj("Handler")]);
    assert_eq!(selected.descriptor, "(LConfig;LHandler;)Ljava/lang/String;");
}

#[test]
fn same_simple_name_in_different_packages_is_not_a_conflict() {
    // Two files declaring a class (and a top-level function) of the same simple name in
    // DIFFERENT packages are distinct declarations, not a "conflicting declarations" clash —
    // exactly the shape produced when the coroutine-helper source (`package helpers`) is
    // injected alongside a box test that redeclares `EmptyContinuation` / `runBlocking` in the
    // root package. The dedup keys are package-qualified, so this collects without error.
    let mut d = DiagSink::new();
    let a = parse_file(
        "class EmptyContinuation\n\
             fun runBlocking() {}\n\
             fun String.packageScopedExtension(): Int = 1",
        &mut d,
    );
    let b = parse_file(
        "package helpers\n\
             class EmptyContinuation\n\
             fun runBlocking() {}\n\
             fun String.packageScopedExtension(): String = \"helpers\"",
        &mut d,
    );
    let files = vec![a, b];
    let symbols = collect_signatures(&files, &mut d);
    let errs: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
    assert!(
        errs.is_empty(),
        "cross-package homonyms wrongly flagged: {errs:?}"
    );
    assert_eq!(
        symbols
            .ext_fun_overloads_in_scope(Ty::String, "packageScopedExtension", &[type_name("")],)
            .first()
            .map(|signature| signature.ret),
        Some(Ty::Int)
    );
    assert_eq!(
        symbols
            .ext_fun_overloads_in_scope(
                Ty::String,
                "packageScopedExtension",
                &[type_name("helpers")],
            )
            .first()
            .map(|signature| signature.ret),
        Some(Ty::String)
    );
}

#[test]
fn same_package_duplicate_class_and_fun_still_conflicts() {
    // The guard stays sound: a genuine same-package duplicate (same internal name / same erased
    // signature) is still reported.
    let mut d = DiagSink::new();
    let a = parse_file("class Dup\nfun f() {}", &mut d);
    let b = parse_file("class Dup\nfun f() {}", &mut d);
    let files = vec![a, b];
    let _ = collect_signatures(&files, &mut d);
    let errs: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
    assert!(
        errs.iter()
            .any(|e| e.contains("conflicting declarations: Dup")),
        "expected class conflict, got {errs:?}"
    );
    assert!(
        errs.iter().any(|e| e == "conflicting overloads:\nfun f()"),
        "expected fun conflict, got {errs:?}"
    );
}

#[test]
fn function_type_arity_distinguishes_erased_overloads() {
    // Extension-function types include their receiver in FunctionN's arity. The signature-clash
    // check must retain that semantic shape instead of reducing both syntactic refs to `<fun>`.
    ok("class Box\n\
            interface Ui {\n\
              fun <T> choose(seed: Any, init: T.() -> Unit): T\n\
              fun choose(seed: Any, init: () -> Unit): Box\n\
              fun mixed(init: Box.() -> Unit): Box\n\
              fun mixed(init: () -> Unit): Box\n\
            }");
}

#[test]
fn subclass_resolves_generic_base_property_type() {
    // A base class declares a member in terms of its OWN type parameter (`val some: T`); a
    // subclass that fixes the type arg (`I : A<String>()`) must not see `T` as an unresolved
    // reference when collect_signatures pulls the base's props into the subclass's inference
    // scope. (Regression: base props were resolved with the SUBCLASS's type params.)
    ok("abstract class A<T> { abstract val some: T }\n\
            class I : A<String>() { override val some: String get() = \"OK\" }\n\
            fun box(): String = I().some");
}

#[test]
fn boxed_array_type_resolves_and_type_checks() {
    // `Array<Int>` is a boxed `Integer[]` (distinct from `IntArray`). The type resolves (it used
    // to error "Array of a primitive"), and element access / size type-check as `Int`.
    ok("fun id(a: Array<Int>): Array<Int> = a\n\
            fun rd(a: Array<Int>): Int = a[0]\n\
            fun wr(a: Array<Int>) { a[0] = 5 }\n\
            fun sz(a: Array<Int>): Int = a.size\n\
            fun box(): String = \"OK\"");
}

#[test]
fn classpath_property_reads_record_resolved_members_for_lowering() {
    let source = "val String.length: String get() = \"bad\"\n\
             fun len(s: String, n: String?): Int = s.length + (n?.length ?: 0)";
    let mut diagnostics = DiagSink::new();
    let cp = std::rc::Rc::new(crate::toolchain::stdlib_and_jdk_classpath());
    let (file, symbols, info) = crate::frontend::analyze_source(
        source,
        jvm_platform(Box::new(initialized_jvm_libraries(cp))),
        &mut diagnostics,
    );
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    let info = info.expect("production frontend must check the source");
    let string = symbols
        .libraries
        .classifier(type_name("kotlin/String"))
        .expect("the platform must publish the String classifier record");
    assert!(
        string
            .declared_callables
            .get("length")
            .is_some_and(|callables| !callables.properties().is_empty()),
        "String.length must be an ordinary property declaration"
    );
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );
    let member = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Member { name, .. } if name == "length" => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain s.length member expression");
    assert!(
        matches!(
            info.resolved_calls.get(&member),
            Some(ResolvedCall::Member(_))
        ),
        "checker must record the selected classpath getter for lowering"
    );
    let safe_call = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::SafeCall {
                name, args: None, ..
            } if name == "length" => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain n?.length safe-call expression");
    assert!(
        matches!(
            info.resolved_calls.get(&safe_call),
            Some(ResolvedCall::Member(_))
        ),
        "checker must record the selected classpath getter for safe-call lowering"
    );
}

#[test]
fn source_enum_entries_property_resolves_for_bare_and_nested_classifiers() {
    ok("enum class Direct { VALUE }\n\
            class Owner { enum class Nested { VALUE } }\n\
            fun inspect() { Direct.entries; Owner.Nested.entries }");
}

/// A source-declared enum must reach the same semantic property seam as a classpath enum. The
/// frontend records the declaring owner and the selected `entries` operation; the backend owns
/// any physical accessor used to realize that operation.
#[test]
fn source_enum_entries_records_the_declaring_owner_and_semantic_operation() {
    let source = "enum class Direct { VALUE }\n\
             class Owner { enum class Nested { VALUE } }\n\
             fun inspect() { Direct.entries; Owner.Nested.entries }";
    let platform = initialized_jvm_libraries(std::rc::Rc::new(
        crate::toolchain::stdlib_and_jdk_classpath(),
    ));
    let (file, info, diagnostics) = retained_platform_analysis(source, Box::new(platform));
    let reads = file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            matches!(expression, Expr::Member { name, .. } if name == "entries")
                .then_some(ExprId(index as u32))
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 2);
    assert_no_diags(&diagnostics);

    let owners = ["Direct", "Owner$Nested"];
    for (read, owner) in reads.into_iter().zip(owners) {
        let Some(ExprLowering::ClassifierPropertyRead {
            owner: recorded,
            property,
        }) = info.expr_lowers.get(&read)
        else {
            panic!("source enum '{owner}' did not record an entries read for lowering");
        };
        assert_eq!(recorded.render(), owner);
        assert_eq!(
            property.operation,
            crate::libraries::ImplicitClassifierProperty::EnumEntries
        );
        assert_eq!(
            property.ty,
            Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj(owner)])
        );
        assert_eq!(
            info.ty(read),
            Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj(owner)])
        );
    }
}

#[test]
fn enum_without_a_direct_entries_accessor_records_the_unavailable_realization() {
    // A provider interns declarations while indexing them; lookup itself only performs a
    // read-only child probe. FakeMemberPlatform has no construction phase, so model that one
    // indexing step explicitly rather than making `symbols` intern every queried spelling.
    let _java_state = crate::types::type_name("test/JavaState");
    let source = "import test.JavaState\nfun inspect() { JavaState.entries }";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    let entries = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| {
            matches!(expression, Expr::Member { name, .. } if name == "entries")
                .then_some(ExprId(index as u32))
        })
        .expect("source should contain the entries property");
    assert_no_diags(&diagnostics);
    assert!(matches!(
        info.expr_lowers.get(&entries),
        Some(ExprLowering::ClassifierPropertyRead { property, .. })
            if property.getter.is_none()
    ));
}

#[test]
fn enum_name_and_ordinal_are_ordinary_inherited_properties() {
    let source = "enum class State { READY }\n\
             fun inspect(state: State): String = state.name + state.ordinal";
    let platform = initialized_jvm_libraries(std::rc::Rc::new(
        crate::toolchain::stdlib_and_jdk_classpath(),
    ));
    let (file, info, diagnostics) = retained_platform_analysis(source, Box::new(platform));
    assert_no_diags(&diagnostics);

    let resolved_properties = file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let Expr::Member { name, .. } = expression else {
                return None;
            };
            matches!(name.as_str(), "name" | "ordinal")
                .then_some((name.as_str(), ExprId(index as u32)))
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved_properties.len(), 2);
    for (name, expression) in resolved_properties {
        let Some(ResolvedCall::Member(target)) = info.resolved_calls.get(&expression) else {
            panic!("enum property '{name}' did not use ordinary member resolution");
        };
        assert_eq!(
            target.ret,
            if name == "name" { Ty::String } else { Ty::Int }
        );
    }
}

#[test]
fn classpath_member_calls_record_resolved_members_for_lowering() {
    let source = "fun direct(s: String): String = s.known(b = 2, a = 1)\n\
             fun String.implicit(): String = known(a = 2)\n\
             fun overloaded(s: String): String = s.choose(a = 1)";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let direct = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Member { name, .. } if name == "known" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain s.known() call");
    assert!(
        matches!(
            info.resolved_calls.get(&direct),
            Some(ResolvedCall::Member(_))
        ),
        "checker must record direct classpath member calls for lowering"
    );
    let direct_slots = info
        .resolved_call_arg_slots
        .get(&direct)
        .expect("direct named member call should record argument slots");
    let direct_values: Vec<_> = direct_slots
        .iter()
        .map(|slot| {
            slot.and_then(|arg| match file.expr(arg) {
                Expr::IntLit(v) => Some(*v),
                _ => None,
            })
        })
        .collect();
    assert_eq!(direct_values, vec![Some(1), Some(2)]);

    let implicit = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Name(name) if name == "known" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain implicit known() call");
    assert!(
        matches!(
            info.resolved_calls.get(&implicit),
            Some(ResolvedCall::Member(_))
        ),
        "checker must record implicit-receiver classpath member calls for lowering"
    );
    let implicit_slots = info
        .resolved_call_arg_slots
        .get(&implicit)
        .expect("implicit defaulted member call should record argument slots");
    let implicit_values: Vec<_> = implicit_slots
        .iter()
        .map(|slot| {
            slot.and_then(|arg| match file.expr(arg) {
                Expr::IntLit(v) => Some(*v),
                _ => None,
            })
        })
        .collect();
    assert_eq!(implicit_values, vec![Some(2), None]);

    let overloaded = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Member { name, .. } if name == "choose" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain s.choose() call");
    let Some(ResolvedCall::Member(selected)) = info.resolved_calls.get(&overloaded) else {
        panic!("checker must record overloaded classpath member calls for lowering");
    };
    assert_eq!(selected.member.params, vec![Ty::Int]);
}

#[test]
fn lambda_literal_extension_defers_an_inapplicable_member_family() {
    let source = "import test.literalChoice\n\
             fun choose(): String = \"\".literalChoice(a = { \"value\" })";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    let call = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| match expression {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Member { name, .. } if name == "literalChoice" => Some(ExprId(index as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain literalChoice call");
    assert_no_diags(&diagnostics);
    let selected = info.resolved_extension(call).unwrap_or_else(|| {
        panic!(
            "lambda-literal Unit coercion must select the extension, got {:?}",
            info.resolved_calls.get(&call)
        )
    });
    assert_eq!(selected.owner, "test/TextKt");
    assert_eq!(selected.name, "literalChoice");
}

#[test]
fn classpath_named_calls_report_invalid_mixing_without_positional_fallback() {
    let source = "import test.mixExt\n\
            fun top(): Int = knownPair(b = 2, 1)\n\
            fun member(): String = \"\".known(b = 2, 1)\n\
            fun extension(): Int = \"\".mixExt(b = 2, 1)";
    let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    let mixing = "mixing named and positional arguments is not allowed unless the order of the arguments matches the order of the parameters.";
    let missing = "no value passed for parameter 'a'.";
    // kotlinc reports in source order. Where the missing argument anchors on the value arguments
    // (2.4.0 and 2.4.10), both diagnostics start at the positional `1` and the mixing error,
    // reported while mapping arguments, comes first; on the callee it precedes the mixing error.
    let expected = ["knownPair", "known", "mixExt"]
        .into_iter()
        .flat_map(|callee| {
            let missing = (missing, missing_argument_anchor("1)", callee), Some(callee));
            let mixing = (mixing, "1", None);
            match crate::diagnostic_wording::no_value_for_parameter_anchor() {
                crate::diagnostic_wording::Anchor::ValueArguments => [mixing, missing],
                crate::diagnostic_wording::Anchor::ReferencedNameByQualified => [missing, mixing],
            }
        })
        .collect::<Vec<_>>();
    let actual = diagnostics
        .diags
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.msg.as_str(),
                &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
                diagnostic
                    .editor_span
                    .map(|span| &source[span.lo as usize..span.hi as usize]),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn classpath_named_calls_reject_conflicting_overload_mappings() {
    let source = "import test.conflictingNamesExt\n\
            fun top(): Int = conflictingNames(a = 1, a = 2)\n\
            fun extension(): Int = \"\".conflictingNamesExt(a = 1, a = 2)";
    let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            [
                "none of the following candidates is applicable:\n\nfun conflictingNames(a: Int, c: Int): Int\nfun conflictingNames(b: String, d: String): Int",
                "none of the following candidates is applicable:\n\nfun String.conflictingNamesExt(a: Int, c: Int): Int\nfun String.conflictingNamesExt(b: String, d: String): Int",
            ],
        );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| { &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize] })
            .collect::<Vec<_>>(),
        ["conflictingNames", "conflictingNamesExt"],
    );
}

#[test]
fn classpath_named_calls_report_mapped_type_errors_without_positional_fallback() {
    let source = "import test.typedExt\n\
            fun top(): String = knownTop(b = \"x\", a = 1)\n\
            fun member(): String = \"\".typedKnown(b = \"x\", a = 1)\n\
            fun extension(): Int = \"\".typedExt(b = \"x\", a = 1)";
    let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    let expected = [
        (
            "argument type mismatch: actual type is 'String', but 'Int' was expected.",
            "\"x\"",
        ),
        (
            "argument type mismatch: actual type is 'Int', but 'String' was expected.",
            "1",
        ),
    ]
    .into_iter()
    .cycle()
    .take(6)
    .collect::<Vec<_>>();
    let actual = diagnostics
        .diags
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.msg.as_str(),
                &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn inapplicable_member_call_lists_the_rejected_overloads() {
    // The member-call last-resort diagnostic must NAME the overloads that rejected the call —
    // the bare prefix alone reads as a compiler bug, not a verdict (the captured-`it`
    // `sink.emit { … }` false positive surfaced with an empty list).
    let source = "fun f(): String = \"\".choose(\"x\")\n";
    let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    assert_eq!(
            diagnostics.diags[0].msg,
            "none of the following candidates is applicable:\n\nfun choose(a: Boolean): String\nfun choose(a: Int): String"
        );
    assert_eq!(
        &source[diagnostics.diags[0].span.lo as usize..diagnostics.diags[0].span.hi as usize],
        "choose"
    );
}

#[test]
fn classpath_named_calls_render_aligned_excess_as_callable_arity_errors() {
    let source = "import test.mixExt\n\
            fun top(): Int = knownPair(a = 1, 2, 3)\n\
            fun member(): String = \"\".known(a = 1, 2, 3)\n\
            fun extension(): Int = \"\".mixExt(a = 1, 2, 3)";
    let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    let expected = [
        "too many arguments for 'fun knownPair(a: Int, b: Int): Int'.",
        "too many arguments for 'fun known(a: Int, b: Int = ...): String'.",
        "too many arguments for 'fun mixExt(a: Int, b: Int): Int'.",
    ];
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        expected,
    );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize])
            .collect::<Vec<_>>(),
        ["3", "3", "3"],
    );
}

#[test]
fn classpath_compare_operator_records_resolved_member_for_lowering() {
    let source = "fun less(a: BoxedComparable, b: BoxedComparable): Boolean = a < b";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let comparison = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Binary { op: BinOp::Lt, .. } => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain a < b comparison");
    let Some(ResolvedCall::Member(selected)) = info
        .resolved_operator_calls
        .get(&(comparison, SyntheticOperatorCall::CompareTo))
    else {
        panic!("checker must record classpath compareTo selected for relational lowering");
    };
    assert_eq!(selected.member.name, "compareTo");
    assert_eq!(selected.ret, Ty::Int);
}

#[test]
fn classpath_index_operator_records_resolved_member_for_lowering() {
    let source = "fun first(xs: BoxedIndex): String = xs[0]";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let index = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Index { .. } => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain xs[0] index expression");
    let Some(ResolvedCall::Member(selected)) = info.resolved_calls.get(&index) else {
        panic!("checker must record classpath get selected for index lowering");
    };
    assert_eq!(selected.member.name, "get");
    assert_eq!(selected.ret, Ty::String);
}

#[test]
fn classpath_string_index_operator_records_resolved_member_for_lowering() {
    let source = "fun first(s: String): Char = s[0]";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let index = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Index { .. } => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain s[0] index expression");
    let Some(ResolvedCall::Member(selected)) = info.resolved_calls.get(&index) else {
        panic!("checker must record classpath String.get selected for index lowering");
    };
    assert_eq!(selected.member.name, "get");
    assert_eq!(selected.ret, Ty::Char);
}

#[test]
fn module_index_operator_records_member_for_lowering() {
    let source = "class Box { operator fun get(i: Int): String = \"x\" }\n\
             fun first(b: Box): String = b[0]";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let index = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Index { .. } => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain b[0] index expression");
    assert!(
        matches!(
            info.resolved_calls.get(&index),
            Some(ResolvedCall::Member(member))
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.owner.is_some_and(|owner| owner.matches("Box"))
                    && member.member.name == "get"
                    && member.member.params == vec![Ty::Int]
                    && member.ret == Ty::String
        ),
        "checker must record same-module member get selected for index lowering"
    );
}

#[test]
fn cross_file_inherited_member_records_declaring_owner() {
    let sources = [
        "open class Base { fun ok(): String = \"O\" }",
        "class Child : Base()\nfun box(): String = Child().ok()",
    ];
    let (files, mut types, diagnostics) = retained_standalone_source_set(&sources);
    let info = types.pop().expect("second source analysis");
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let call = files[1]
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match files[1].expr(*callee) {
                Expr::Member { name, .. } if name == "ok" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain Child().ok() call");

    assert!(
        matches!(
            info.resolved_calls.get(&call),
            Some(ResolvedCall::Member(member))
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.owner.is_some_and(|owner| owner.matches("Base"))
                    && member.member.name == "ok"
                    && member.member.params.is_empty()
                    && member.ret == Ty::String
                    && !member.member.is_interface()
        ),
        "checker must record the declaring module owner for inherited member calls"
    );
}

#[test]
fn declared_property_resolution_prefers_the_class_hierarchy_over_interfaces() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "interface Contract { val value: String }\n\
             open class Base {\n\
                 var value: String = \"base\"\n\
                     private set\n\
             }\n\
             class Child : Base(), Contract",
        &mut diagnostics,
    );
    let files = vec![file];
    let symbols = collect_signatures(&files, &mut diagnostics);
    assert_no_diags(&diagnostics);

    let child = symbols
        .classes
        .get(&type_name("Child"))
        .expect("child signature")
        .internal_name();
    let (owner, property) = symbols
        .declared_member_prop(child, "value")
        .expect("inherited value declaration");

    // `Base.value` is the concrete class member that satisfies `Contract.value`. Property
    // lookup, callable-reference lowering, and access checks must therefore all retain Base's
    // mutability and private-setter facts; selecting the interface declaration here would make
    // an unsafe mutable reference appear publicly writable.
    assert!(owner.matches("Base"), "selected owner: {}", owner.render());
    assert!(property.setter_name.is_some());
    assert_eq!(property.setter_visibility, Some(Visibility::Private));
}

#[test]
fn module_index_operator_records_extension_for_lowering() {
    let source = "class Box\n\
             operator fun Box.get(i: Int): String = \"x\"\n\
             fun first(b: Box): String = b[0]";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let index = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Index { .. } => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain b[0] index expression");
    assert!(
        matches!(
            info.resolved_calls.get(&index),
            Some(ResolvedCall::Extension(extension))
                if extension.receiver == Ty::obj("Box")
                    && extension.callable.name == "get"
                    && extension.params == vec![Ty::Int]
                    && extension.callable.ret == Ty::String
        ),
        "checker must record same-module extension get selected for index lowering"
    );
}

#[test]
fn rejected_reference_range_does_not_record_partial_operator_calls() {
    let source = "class VR {\n\
             \x20 operator fun contains(v: V): Int = 1\n\
             }\n\
             class V {\n\
             \x20 operator fun rangeTo(o: V): VR = VR()\n\
             }\n\
             fun box(): Boolean = V() in V()..V()";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    assert_eq!(
        diagnostics.diags[0].msg,
        "operator 'contains' cannot be applied to range 'VR' and 'V'"
    );
    assert_eq!(
        &source[diagnostics.diags[0].span.lo as usize..diagnostics.diags[0].span.hi as usize],
        "V() in V()..V()"
    );

    let in_range = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::InRange { start, .. } if info.ty(*start) == Ty::obj("V") => {
                Some(ExprId(idx as u32))
            }
            _ => None,
        })
        .expect("source should contain reference in-range expression");

    assert_eq!(info.ty(in_range), Ty::Error);
    assert!(
        info.resolved_operator_call(in_range, "rangeTo").is_none(),
        "rejected in-range expression must not publish partial rangeTo resolution"
    );
    assert!(
        info.resolved_operator_call(in_range, "contains").is_none(),
        "rejected in-range expression must not publish invalid contains resolution"
    );
}

#[test]
fn index_assignment_records_member_set_for_lowering() {
    let source = "class Box { operator fun set(i: Int, v: String) {} }\n\
             fun write(b: Box) { b[0] = \"x\" }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let assign = file
        .stmt_arena
        .iter()
        .enumerate()
        .find_map(|(idx, stmt)| match stmt {
            Stmt::AssignIndex { .. } => Some(StmtId(idx as u32)),
            _ => None,
        })
        .expect("source should contain indexed assignment");

    assert!(
        matches!(
            info.resolved_stmt_operator_call(assign, "set"),
            Some(ResolvedCall::Member(member))
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.owner.is_some_and(|owner| owner.matches("Box"))
                    && member.member.name == "set"
                    && member.member.params.as_slice() == [Ty::Int, Ty::String]
                    && member.ret == Ty::Unit
        ),
        "checker must record member set selected for indexed-assignment lowering"
    );
}

#[test]
fn index_assignment_records_extension_set_for_lowering() {
    let source = "class Box\n\
             operator fun Box.set(i: Int, v: String) {}\n\
             fun write(b: Box) { b[0] = \"x\" }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let assign = file
        .stmt_arena
        .iter()
        .enumerate()
        .find_map(|(idx, stmt)| match stmt {
            Stmt::AssignIndex { .. } => Some(StmtId(idx as u32)),
            _ => None,
        })
        .expect("source should contain indexed assignment");

    assert!(
        matches!(
            info.resolved_stmt_operator_call(assign, "set"),
            Some(ResolvedCall::Extension(extension))
                if extension.receiver == Ty::obj("Box")
                    && extension.callable.name == "set"
                    && extension.params.as_slice() == [Ty::Int, Ty::String]
                    && extension.callable.ret == Ty::Unit
        ),
        "checker must record extension set selected for indexed-assignment lowering"
    );
}

#[test]
fn index_assignment_records_get_return_for_lowering_guard() {
    let source = "fun write(b: BoxedIndex) { b[0] = \"x\" }";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_no_diags(&diagnostics);

    let assign = file
        .stmt_arena
        .iter()
        .enumerate()
        .find_map(|(idx, stmt)| match stmt {
            Stmt::AssignIndex { .. } => Some(StmtId(idx as u32)),
            _ => None,
        })
        .expect("source should contain indexed assignment");

    assert_eq!(
        info.resolved_index_store_get_return(assign),
        Some(Ty::String),
        "checker must record get(Int) return used by indexed-assignment lowering guard"
    );
}

#[test]
fn foreach_records_iterator_protocol_for_lowering() {
    let source = "fun loop(xs: BoxedIterable) { for (x in xs) { val y = x } }";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_no_diags(&diagnostics);

    let iterable = file
        .stmt_arena
        .iter()
        .find_map(|stmt| match stmt {
            Stmt::ForEach { iterable, .. } => Some(*iterable),
            _ => None,
        })
        .expect("source should contain foreach statement");
    let protocol = info
        .iterator_protocol(iterable)
        .expect("checker must record foreach iterator protocol");

    assert_eq!(protocol.elem_ty, Ty::String);
    assert_eq!(
        protocol.iter_ty,
        Ty::obj_args("BoxedIterator", &[Ty::String])
    );
    assert!(matches!(
        protocol.has_next.as_ref(),
        ResolvedCall::Member(resolved) if resolved.member.name == "hasNext"
    ));
    assert!(matches!(
        protocol.next.as_ref(),
        ResolvedCall::Member(resolved) if resolved.member.name == "next"
    ));
    assert!(
        matches!(
            protocol.iterator.as_ref(),
            ResolvedCall::Member(resolved)
                if resolved.member.owner.is_some_and(|owner| owner.matches("BoxedIterable"))
                    && resolved.member.name == "iterator"
        ),
        "checker must record the member iterator dispatch selected for foreach lowering"
    );
}

#[test]
fn destructuring_records_member_component_for_lowering() {
    let source = "class Box(val s: String) { operator fun component1(): String = s }\n\
             fun read(b: Box): String { val (x) = b; return x }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let stmt = file
        .stmt_arena
        .iter()
        .enumerate()
        .find_map(|(idx, stmt)| match stmt {
            Stmt::Destructure { .. } => Some(StmtId(idx as u32)),
            _ => None,
        })
        .expect("source should contain destructuring statement");

    assert!(
        matches!(
            info.resolved_destructure_component(stmt, 0),
            Some(target)
                if matches!(
                    target.as_ref(),
                    ResolvedCall::Member(member)
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.owner.is_some_and(|owner| owner.matches("Box"))
                    && member.member.name == "component1"
                    && member.member.params.is_empty()
                    && member.ret == Ty::String
                )
        ),
        "checker must record member component selected for destructuring lowering"
    );
}

#[test]
fn destructuring_records_extension_component_for_lowering() {
    let source = "class Box(val s: String)\n\
             operator fun Box.component1(): String = s\n\
             fun read(b: Box): String { val (x) = b; return x }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let stmt = file
        .stmt_arena
        .iter()
        .enumerate()
        .find_map(|(idx, stmt)| match stmt {
            Stmt::Destructure { .. } => Some(StmtId(idx as u32)),
            _ => None,
        })
        .expect("source should contain destructuring statement");

    assert!(
        matches!(
            info.resolved_destructure_component(stmt, 0),
            Some(target)
                if matches!(
                    target.as_ref(),
                    ResolvedCall::Extension(extension)
                        if extension.receiver == Ty::obj("Box")
                            && extension.callable.name == "component1"
                            && extension.params.is_empty()
                            && extension.callable.ret == Ty::String
                )
        ),
        "checker must record extension component selected for destructuring lowering"
    );
}

#[test]
fn counted_range_loop_destructures_its_synthetic_iteration_value() {
    let errors = check_with_detected_features(
        "// LANGUAGE: +NameBasedDestructuring +EnableNameBasedDestructuringShortForm\n\
             operator fun Int.component1(): Int = this + 1\n\
             operator fun Int.component2(): Int = this + 2\n\
             fun read(): Int {\n\
                 var sum = 0\n\
                 for ([a, b] in 0..2) { sum += a + b }\n\
                 return sum\n\
             }",
    );
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn name_based_destructuring_records_property_getter_for_lowering() {
    let source = "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
             data class P(val first: Int, val second: String)\n\
             fun read(p: P): String { val (text = second) = p; return text }";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let stmt = file
        .stmt_arena
        .iter()
        .enumerate()
        .find_map(|(idx, stmt)| match stmt {
            Stmt::Destructure { .. } => Some(StmtId(idx as u32)),
            _ => None,
        })
        .expect("source should contain destructuring statement");

    assert!(
        matches!(
            info.resolved_destructure_component(stmt, 0),
            Some(target)
                if matches!(
                    target.as_ref(),
                    ResolvedCall::Member(member)
                        if member.member.owner.is_some_and(|owner| owner.matches("P"))
                            && member.member.name == "getSecond"
                            && member.ret == Ty::String
                )
        ),
        "checker must record the unified selected property member for name-based destructuring"
    );
}

#[test]
fn name_based_destructuring_specializes_a_generic_property_from_the_receiver() {
    let errors = check_with_detected_features(
        "// LANGUAGE: +NameBasedDestructuring\n\
             open class Base\n\
             class Leaf : Base()\n\
             class Box<T : Base>(val value: T)\n\
             fun pick(value: Leaf): String = \"leaf\"\n\
             fun pick(value: Base): String = \"base\"\n\
             fun test(box: Box<Leaf>): String { (val value) = box; return pick(value) }",
    );
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn integer_literal_adaptation_is_consistent_across_source_call_origins() {
    ok("fun topLevel(value: Long): Long = value\n\
             class Owner { fun member(value: Long): Long = value }\n\
             fun String.extension(value: Long): Long = value\n\
             fun unsigned(value: ULong): ULong = value\n\
             fun useTopLevel(): Long = topLevel(1)\n\
             fun useMember(): Long = Owner().member(1 + 1)\n\
             fun useExtension(): Long = \"\".extension(-1)\n\
             fun useUnsigned(): ULong = unsigned(1u)");
}

#[test]
fn context_property_getter_sees_its_named_parameter() {
    let mut diagnostics = DiagSink::new();
    let source = "class Scope\n\
             context(scope: Scope)\n\
             val current: Scope get() = scope";
    let _ = crate::frontend::analyze_source_standalone(source, &mut diagnostics);

    assert_no_diags(&diagnostics);
}

#[test]
fn context_property_getter_infers_from_its_parameter() {
    let mut diagnostics = DiagSink::new();
    let source = "class Scope\n\
             context(scope: Scope)\n\
             val current get() = scope";
    let (_, symbols, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");

    assert_no_diags(&diagnostics);
    assert_eq!(
        symbols
            .source_props
            .values()
            .find(|property| property.name == "current")
            .map(|property| property.ty),
        Some(Ty::obj("Scope"))
    );
}

#[test]
fn member_context_property_getter_sees_its_parameter() {
    let mut diagnostics = DiagSink::new();
    let source = "class Scope\n\
             class Owner {\n\
                 context(scope: Scope)\n\
                 val current get() = scope\n\
             }";
    let (_, symbols, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");

    assert_no_diags(&diagnostics);
    assert_eq!(
        symbols
            .classes
            .get(&type_name("Owner"))
            .and_then(|class| class.contextual_props("current").first())
            .map(|property| property.ty),
        Some(Ty::obj("Scope"))
    );
}

#[test]
fn anonymous_context_property_parameters_may_repeat() {
    let mut diagnostics = DiagSink::new();
    let source = "class Scope\n\
             context(_: Scope, _: Scope)\n\
             val current: Int get() = 1";
    let _ = crate::frontend::analyze_source_standalone(source, &mut diagnostics);

    assert_no_diags(&diagnostics);
}

#[test]
fn context_property_rejects_duplicate_parameter_names() {
    err_contains(
        "class Scope\n\
             context(scope: Scope, scope: Scope)\n\
             val current: Scope get() = scope",
        "conflicting declaration: parameter 'scope' is declared more than once",
    );
}

#[test]
fn context_property_rejects_backing_field() {
    err_contains(
        "class Scope\n\
             context(scope: Scope)\n\
             val current: Scope = scope",
        "context property cannot have a backing field",
    );
}

#[test]
fn context_property_access_supplies_its_context_accessor() {
    assert!(check(
        "class Scope\n\
             context(scope: Scope)\n\
             val current: Scope get() = scope\n\
             context(scope: Scope)\n\
             fun read(): Scope = current"
    )
    .0
    .is_empty());
}

#[test]
fn member_context_property_access_supplies_its_context_accessor() {
    assert!(check(
        "class Scope\n\
             class Owner {\n\
                 context(scope: Scope)\n\
                 val current: Scope get() = scope\n\
             }\n\
             context(scope: Scope)\n\
             fun read(owner: Owner): Scope = owner.current"
    )
    .0
    .is_empty());
}

#[test]
fn context_extension_property_access_supplies_its_context_accessor() {
    assert!(check(
        "class Scope\n\
             class Owner\n\
             context(scope: Scope)\n\
             val Owner.current: Scope get() = scope\n\
             context(scope: Scope)\n\
             fun read(owner: Owner): Scope = owner.current"
    )
    .0
    .is_empty());
}

#[test]
fn module_top_level_context_overload_skips_unsatisfied_candidate() {
    let source = "class A(val x: String)\n\
             class B(val x: String)\n\
             context(a: A) fun leaf(): String = a.x\n\
             context(b: B) fun leaf(): String = b.x\n\
             context(b: B) fun mid(): String = leaf()";
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let call = named_call(&file, "leaf");
    let target = module_top_level_target(&info, call);
    assert_eq!(target.callable.params, vec![Ty::obj("B")]);
    assert!(matches!(
        target.context_args.as_slice(),
        [Some(ResolvedContextArgument::Binding {
            name,
            source: ContextBindingSource::ProvenLocalDepth {
                shadow_depth: 0,
                lexical_binding: Some(_),
            },
            flow_identity: Some(_),
        })] if name == "b"
    ));
}

#[test]
fn classpath_top_level_named_calls_record_arg_slots_for_lowering() {
    let source = "fun direct(): String = knownTop(b = 2, a = \"x\")";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let call = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Name(name) if name == "knownTop" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain knownTop() call");
    assert!(
        matches!(
            info.resolved_calls.get(&call),
            Some(ResolvedCall::TopLevel(_))
        ),
        "checker must record the selected classpath top-level callable for lowering"
    );
    let slots = info
        .resolved_call_arg_slots
        .get(&call)
        .expect("classpath top-level named call should record argument slots");
    let values: Vec<_> = slots
        .iter()
        .map(|slot| {
            slot.map(|arg| match file.expr(arg) {
                Expr::StringLit(v) => v.to_lossy(),
                Expr::IntLit(v) => v.to_string(),
                other => panic!("unexpected argument expression in slot: {other:?}"),
            })
        })
        .collect();
    assert_eq!(values, vec![Some("x".to_string()), Some("2".to_string())]);
}

#[test]
fn unknown_named_argument_diagnostic_is_consistent_across_call_origins() {
    let cases = [
        "fun bad(): String = knownTop(z = \"x\")",
        "fun bad(): String = \"\".known(z = 1)",
        "import test.tag\nfun bad(): String = \"\".tag(z = 1)",
        "class Outer { class Inner(val a: Int) }\n\
             fun bad(): Outer.Inner = Outer.Inner(z = 1)",
    ];
    for source in cases {
        let (_, _, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));

        assert_eq!(
            diagnostics.diags.len(),
            1,
            "{source}: {:?}",
            diagnostics.diags
        );
        let diagnostic = &diagnostics.diags[0];
        assert_eq!(diagnostic.msg, "no parameter with name 'z' found.");
        assert_eq!(
            &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
            "z"
        );
    }
}

#[test]
fn classpath_top_level_function_refs_record_callable_for_lowering() {
    let source = "val ref = ::knownTop";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let function_ref = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::CallableRef {
                receiver: None,
                name,
            } if name == "knownTop" => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain ::knownTop");
    let Some(ExprLowering::TopLevelFunctionRef(reference)) = info.expr_lowers.get(&function_ref)
    else {
        panic!("checker must record classpath top-level function refs for lowering");
    };
    assert!(reference.target.owner.matches("test/TopKt"));
    assert_eq!(reference.target.name, "knownTop");
    assert_eq!(reference.target.params, vec![Ty::String, Ty::Int]);
}

#[test]
fn nullable_classifier_reference_selects_nullable_extension_before_instance_member() {
    let source = "class C { fun pick(value: Int): String = \"member\" }\n\
             fun C?.pick(value: Int): String = \"extension\"\n\
             fun use(value: C?): String = (C?::pick)(value, 1)";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_no_diags(&diagnostics);

    let reference = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| {
            let id = ExprId(index as u32);
            matches!(expression, Expr::CallableRef { name, .. } if name == "pick")
                .then_some(id)
                .filter(|id| file.nullable_callable_ref_receivers.contains(&id.0))
        })
        .expect("source should contain C?::pick");
    assert!(matches!(
        info.expr_lowers.get(&reference),
        Some(ExprLowering::CallableReference {
            binding: CallableReferenceBinding::Unbound,
            target: CallableReferenceTarget::Extension { .. },
        })
    ));
}

#[test]
fn widening_cast_in_earlier_condition_does_not_widen_stable_property_read() {
    let mut diagnostics = DiagSink::new();
    let source = "val PREFIX = \"prefix\"\n\
             fun consume(value: String): String = value\n\
             fun use(): String {\n\
                 if ((PREFIX as String?) == \"ignored\") return \"early\"\n\
                 return consume(PREFIX)\n\
             }";
    let _ = crate::frontend::analyze_source(
        source,
        jvm_platform(Box::new(FakeMemberPlatform)),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
}

#[test]
fn production_checker_consumes_normalized_member_property_stability() {
    let (errors, _) = check(
        "fun consume(value: String) {}\n\
             class Box(val value: Any?) {\n\
                 fun use() { if (value is String) consume(value) }\n\
             }",
    );
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn source_function_supertype_keeps_its_callable_shape() {
    let mut diagnostics = DiagSink::new();
    let source = "// LANGUAGE: +FunctionalTypeWithExtensionAsSupertype +RefinedVarargConversionRulesForCallableReferences\n\
             import kotlin.coroutines.*\n\
             fun accept(block: suspend Int.() -> Unit) {}\n\
             class A : Int.() -> Unit { override fun invoke(value: Int) {} }\n\
             fun arrayTest(f: (Int, Int) -> Array<Int>) {}\n\
             fun charArrayTest(f: (CharArray) -> CharArray) {}\n\
             fun foo(x: String, y: Char = 'K'): String = x + y\n\
             fun <T, U> call(f: (T) -> U, x: T): U = f(x)\n\
             fun executeSuspend(f: suspend () -> Any?): String = \"\"\n\
             fun propertyTest(p: kotlin.reflect.KProperty0<Any?>) { executeSuspend(p) }\n\
             val propertyValue: Int = 1\n\
             fun <T> secondArg(f: (Any, T) -> String): String = \"\"\n\
             fun collectAny(vararg values: Any): String = \"\"\n\
             class BoundC { fun ffff(i: Int, s: String = \"OK\"): String = s }\n\
             abstract class GenericBase<T : GenericVm> { lateinit var viewModel: T }\n\
             abstract class GenericVm\n\
             class GenericVmValue : GenericVm() { var property: String? = \"OK\" }\n\
             class GenericDerived : GenericBase<GenericVmValue>() {\n\
                 fun <T> bind(value: () -> T?): String = \"\"\n\
                 fun boundGenericProperty(): String = bind(viewModel::property)\n\
             }\n\
             fun nullableStart(c: (suspend () -> Unit)?) { c?.startCoroutine(null!!) }\n\
             class Outer(val o: String) {\n\
                 inner class Inner1(val i: Int, vararg v: String) {\n\
                     val result = \"I1\" + o + i + if (v.size == 0) \"E\" else v[0]\n\
                 }\n\
                 inner class Inner2(val i: Int, vararg v: String = arrayOf(\"A\")) {\n\
                     val result = \"I2\" + o + i + v[0]\n\
                 }\n\
             }\n\
             fun <T> use0(f: (Int) -> T) = f(11)\n\
             fun <T> use1(f: (Int, String) -> T) = f(12, \"B\")\n\
             fun boundAdapted(): String = 42.run(BoundC()::ffff)\n\
             fun use() {\n\
                 val oouter = Outer(\"O\")\n\
                 accept((::A)()); arrayTest(::arrayOf); charArrayTest(::charArrayOf); call(::foo, \"O\")\n\
                 propertyTest(::propertyValue); secondArg(::collectAny)\n\
                 val boundAdapted = boundAdapted()\n\
                 val r1 = use0(oouter::Inner1).result\n\
                 val r2 = use1(oouter::Inner1).result\n\
                 val r3 = use0(oouter::Inner2).result\n\
                 val r4 = use1(oouter::Inner2).result\n\
             }";
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB\n// WITH_COROUTINES");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let platform = initialized_jvm_libraries(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(classpath),
    ));
    let inputs = [crate::frontend::SourceInput::kotlin(source)];
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        jvm_platform(Box::new(platform)),
        &LangFeatures::from_source(source),
        &mut diagnostics,
    );
    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let class_declaration = index
        .classifier_declaration(type_name("A"))
        .expect("stable source class A");
    let class_header = index
        .classifier_header(class_declaration)
        .expect("resolved source class A header");
    assert_eq!(
        class_header
            .interfaces
            .iter()
            .map(|interface| interface.get())
            .collect::<Vec<_>>(),
        [Ty::fun_with_shape(vec![Ty::Int], Ty::Unit, 0, true, false,)],
        "stable classifier header lost the declared receiver-function supertype"
    );
    let file = analysis.files.pop().expect("source file");
    let symbols = analysis.symbols;
    let _ = analysis
        .types
        .pop()
        .flatten()
        .expect("production frontend must check the source");
    let files = vec![file];
    for function in ["arrayTest", "charArrayTest", "call"] {
        let signature = symbols
            .single_fun(function)
            .unwrap_or_else(|| panic!("missing source signature for {function}"));
        assert!(
            matches!(
                signature.params.first().map(|ty| ty.kind()),
                Some(TyKind::Fun(_))
            ),
            "{function} lost its semantic function parameter: {signature:#?}"
        );
    }
    {
        let scope = CheckerScope::root();
        let mut probe_diagnostics = DiagSink::new();
        let mut checker =
            make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);
        for (name, expected) in [
            (
                "arrayOf",
                symbols
                    .single_fun("arrayTest")
                    .expect("arrayTest signature")
                    .params[0],
            ),
            (
                "charArrayOf",
                symbols
                    .single_fun("charArrayTest")
                    .expect("charArrayTest signature")
                    .params[0],
            ),
            ("foo", Ty::fun(vec![Ty::String], Ty::String)),
        ] {
            let reference = files[0]
                .expr_arena
                .iter()
                .enumerate()
                .find_map(|(index, expression)| match expression {
                    Expr::CallableRef {
                        receiver: None,
                        name: candidate,
                    } if candidate == name => Some(ExprId(index as u32)),
                    _ => None,
                })
                .expect("callable reference must be parsed");
            let resolved = checker.callable_ref_function_type(
                &scope,
                reference,
                Some(expected),
                None,
                name.to_string(),
            );
            assert!(
                    resolved.is_fun(),
                    "direct real-classpath resolution failed for ::{name}, expected={expected:?}: {:#?}",
                    probe_diagnostics.diags
                );
        }
        let property_reference = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(index, expression)| match expression {
                Expr::CallableRef {
                    receiver: None,
                    name,
                } if name == "propertyValue" => Some(ExprId(index as u32)),
                _ => None,
            })
            .expect("property reference must be parsed");
        let property_expected = symbols
            .single_fun("propertyTest")
            .expect("propertyTest signature")
            .params[0];
        let property_nominal = checker.expr_expected(&scope, property_reference, property_expected);
        let property_function = checker
            .expression_function_type(&scope, property_reference, property_nominal)
            .expect("top-level property reference must retain its callable signature");
        let suspend_expected = symbols
            .single_fun("executeSuspend")
            .expect("executeSuspend signature")
            .params[0];
        assert!(
                checker
                    .member_argument_score(suspend_expected, property_function)
                    .is_some(),
                "property callable does not fit suspend parameter: expected={suspend_expected:?} nominal={property_nominal:?} function={property_function:?}"
            );
        let start_receiver = Ty::fun_suspend(Vec::new(), Ty::Unit);
        let raw_start_overloads = checker
            .resolver()
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::TopLevel,
                "startCoroutine",
                &[],
                &[],
            )
            .map(crate::symbol_resolver::Symbol::overloads)
            .unwrap_or_default();
        let raw_start_shapes = raw_start_overloads
            .iter()
            .map(|candidate| {
                (
                    candidate.kind,
                    candidate.semantic_receiver(),
                    candidate.semantic_params().to_vec(),
                    candidate
                        .generic_sig
                        .as_ref()
                        .map_or(candidate.callable.ret, |signature| signature.ret),
                )
            })
            .collect::<Vec<_>>();
        let start_overloads = checker
            .resolver()
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(start_receiver),
                "startCoroutine",
                &[],
                &[],
            )
            .map(crate::symbol_resolver::Symbol::overloads)
            .unwrap_or_default();
        assert!(
                !start_overloads.is_empty(),
                "startCoroutine metadata has no extension applicable to {start_receiver:?}; raw={raw_start_shapes:#?}"
            );
    }
    assert_no_diags(&diagnostics);
}

#[test]
fn applied_hierarchy_records_the_shortest_diamond_path() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "interface Root\n\
             interface Short : Root\n\
             interface Mid : Root\n\
             interface Deep : Mid\n\
             class Value : Deep, Short",
        &mut diagnostics,
    );
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);

    let hierarchy = symbols.applied_hierarchy(Ty::obj("Value"));
    assert_eq!(
        hierarchy
            .iter()
            .find(|(owner, _, _)| owner.matches("Root"))
            .map(|(_, _, depth)| *depth),
        Some(2)
    );
}

#[test]
fn applied_common_supertype_rejects_equal_depth_semantic_owners() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "interface First\n\
             interface Second\n\
             class Left : First, Second\n\
             class Right : First, Second",
        &mut diagnostics,
    );
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        applied_common_supertype(
            &checker.fed_source(),
            &checker,
            Ty::obj("Left"),
            Ty::obj("Right")
        ),
        None
    );
}

#[test]
fn applied_common_supertype_rejects_incomparable_owners_at_different_depths() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "interface First\n\
             interface Second\n\
             interface ViaSecond : Second\n\
             class Left : First, ViaSecond\n\
             class Right : First, ViaSecond",
        &mut diagnostics,
    );
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        applied_common_supertype(
            &checker.fed_source(),
            &checker,
            Ty::obj("Left"),
            Ty::obj("Right")
        ),
        None
    );
}

#[test]
fn applied_common_supertype_removes_transitively_dominated_owner() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "interface Root\n\
             interface Mid : Root\n\
             interface Leaf : Mid\n\
             class Left : Leaf\n\
             class Right : Leaf",
        &mut diagnostics,
    );
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        applied_common_supertype(
            &checker.fed_source(),
            &checker,
            Ty::obj("Left"),
            Ty::obj("Right")
        ),
        Some(Ty::obj("Leaf"))
    );
}

#[test]
fn joining_null_preserves_the_type_parameter_occurrence() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file("", &mut diagnostics);
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let mut checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);
    let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));

    assert_eq!(
        checker.join(parameter, Ty::Null, Span::new(0, 0)),
        Ty::nullable(parameter)
    );
    assert_no_diags(&probe_diagnostics);
}

#[test]
fn same_invariant_classifier_join_reconstructs_an_out_projection() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file("class A\nclass B\nclass Inv<T>", &mut diagnostics);
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let mut checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        checker.join(
            Ty::obj_args("Inv", &[Ty::obj("A")]),
            Ty::obj_args("Inv", &[Ty::obj("B")]),
            Span::new(0, 0),
        ),
        Ty::obj_args("Inv", &[Ty::out_projection(Ty::obj("kotlin/Any"))])
    );
    assert_no_diags(&probe_diagnostics);
}

#[test]
fn same_covariant_classifier_join_reconstructs_its_argument() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file("class A\nclass B\ninterface Cov<out T>", &mut diagnostics);
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let mut checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        checker.join(
            Ty::obj_args("Cov", &[Ty::obj("A")]),
            Ty::obj_args("Cov", &[Ty::obj("B")]),
            Span::new(0, 0),
        ),
        Ty::obj_args("Cov", &[Ty::obj("kotlin/Any")])
    );
    assert_no_diags(&probe_diagnostics);
}

#[test]
fn same_contravariant_classifier_join_meets_at_the_argument_intersection() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file("class A\nclass B\ninterface Contra<in T>", &mut diagnostics);
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let mut checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        checker.join(
            Ty::obj_args("Contra", &[Ty::obj("A")]),
            Ty::obj_args("Contra", &[Ty::obj("B")]),
            Span::new(0, 0),
        ),
        Ty::obj_args("Contra", &[Ty::intersection(&[Ty::obj("A"), Ty::obj("B")])])
    );
    assert_no_diags(&probe_diagnostics);
}

#[test]
fn projected_generic_sam_lambda_keeps_all_classifier_parameter_bounds() {
    let source = r#"
interface X { fun foo() {} }
interface Z { fun bar() {} }
class A : X, Z
class B : X, Z
fun interface Sink<T> where T : X, T : Z { fun accept(value: T) }
class G<T> where T : X, T : Z {
    fun check(sink: Sink<in T>) {}
}
fun <T> select(left: T, right: T): T = left
fun use() {
    val projected = select(G<A>(), G<B>())
    projected.check {
        it.foo()
        it.bar()
    }
}
"#;
    let mut diagnostics = DiagSink::new();
    let _ = crate::frontend::analyze_source_standalone(source, &mut diagnostics);

    assert_no_diags(&diagnostics);
}

#[test]
fn unrelated_platform_references_join_to_platform_any() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file("class A\nclass B", &mut diagnostics);
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let mut checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        checker.join(
            Ty::platform_nullable(Ty::obj("A")),
            Ty::obj("B"),
            Span::new(0, 0),
        ),
        Ty::platform_nullable(Ty::obj("kotlin/Any"))
    );
    assert_eq!(
        checker.join(
            Ty::platform_nullable(Ty::obj("A")),
            Ty::nullable(Ty::obj("B")),
            Span::new(0, 0),
        ),
        Ty::nullable(Ty::obj("kotlin/Any"))
    );
    assert_no_diags(&probe_diagnostics);
}

#[test]
fn recursive_common_supertype_terminates_with_a_star_projection() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "interface Base<T>\nclass A : Base<A>\nclass B : Base<B>",
        &mut diagnostics,
    );
    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_no_diags(&diagnostics);
    let mut probe_diagnostics = DiagSink::new();
    let checker = make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);

    assert_eq!(
        applied_common_supertype(&checker.fed_source(), &checker, Ty::obj("A"), Ty::obj("B")),
        Some(Ty::obj_args(
            "Base",
            &[Ty::out_projection(Ty::nullable(Ty::obj("kotlin/Any")))]
        ))
    );
    assert_no_diags(&probe_diagnostics);
}

#[test]
fn primitive_array_initializer_accepts_callable_expressions() {
    let (errors, _) = check(
        "fun element(index: Int): Char = 'O'\n\
             class Elements {\n\
                 fun element(index: Int): Char = 'K'\n\
             }\n\
             fun use(): Char {\n\
                 val unbound = CharArray(1, ::element)\n\
                 val bound = CharArray(1, Elements()::element)\n\
                 return unbound[0]\n\
             }",
    );

    assert!(
        errors.is_empty(),
        "callable expressions must use the primitive-array initializer signature: {errors:?}"
    );
}

#[test]
fn bound_extension_reference_applies_defaults_and_empty_vararg() {
    let (errors, _) = check(
        "fun consume(reference: () -> Unit) { reference() }\n\
             fun String.id(value: String = this, vararg rest: Int): String = value\n\
             fun use() { consume(\"value\"::id) }",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn bound_generic_extension_reference_specializes_from_receiver() {
    let (errors, _) = check(
        "fun <T> T.identity() = this\n\
             fun use() {\n\
                 val function: (String) -> String = { it }\n\
                 val reference = function::identity\n\
                 reference()(\"OK\")\n\
             }",
    );

    assert!(
            errors.is_empty(),
            "a bound generic extension reference must retain the receiver-specialized return: {errors:?}"
        );
}

#[test]
fn explicit_spread_accepts_normalized_vararg_element_type() {
    let (errors, _) = check(
        "fun consume(vararg values: String) {}\n\
             fun forward(vararg values: String) { consume(*values) }",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn receiver_function_value_shapes_nested_receiver_lambda_once() {
    let (errors, _) = check(
        "class A(val value: String)\n\
             fun use(block: A.(A.() -> String) -> String): String {\n\
                 return A(\"OK\").block { value }\n\
             }",
    );

    assert!(
        errors.is_empty(),
        "receiver function values must shape their arguments before checking: {errors:?}"
    );
}

#[test]
fn explicit_receiver_function_invoke_shapes_nested_receiver_lambda() {
    let (errors, _) = check(
        "class A(val value: String)\n\
             fun A.use(block: A.(A.() -> String) -> String): String {\n\
                 return block.invoke(this) { value }\n\
             }",
    );

    assert!(
        errors.is_empty(),
        "an explicit invoke call must take its argument shape from the receiver value: {errors:?}"
    );
}

#[test]
fn extension_function_property_call_shapes_nested_receiver_lambda() {
    let (errors, _) = check(
            "class A(val value: String)\n\
             val A.block: A.(A.() -> String) -> String\n\
                 get() = { receiver: A, body: A.()-> String -> receiver.body() } as A.(A.() -> String) -> String\n\
             fun use(): String = A(\"outer\").block(A(\"OK\"), { this.value })",
        );

    assert!(
            errors.is_empty(),
            "a selected function-valued property must shape invoke arguments before checking: {errors:?}"
        );
}

#[test]
fn expected_call_result_selects_generic_callable_reference_overload() {
    let (errors, _) = check(
        "fun <T> choose(reference: (T) -> Unit): T? = null\n\
             fun pick(value: Int?) {}\n\
             fun pick(value: String?) {}\n\
             val selected: Int? = choose(::pick)",
    );

    assert!(
        errors.is_empty(),
        "the outer result must bind its generic before reference overload selection: {errors:?}"
    );
}

#[test]
fn explicit_generic_binding_wins_over_provisional_result_expectation() {
    let (errors, _) = check(
        "fun <T> upcast(value: T): T = value\n\
             fun make(value: Int): Int = value\n\
             fun use() { upcast<(Int) -> Int>(::make)(1) }",
    );

    assert!(
        errors.is_empty(),
        "result inference must not overwrite an explicit generic callable shape: {errors:?}"
    );
}

#[test]
fn source_member_call_uses_common_default_argument_mapping() {
    let (errors, _) = check(
        "class C { fun range(from: Int = 0, to: Int = 100): Int = to - from }\n\
             fun use(): Int = C().range()",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn classifier_value_call_uses_companion_member_candidates() {
    let (errors, _) = check(
        "interface Factory { fun create(): String }\n\
             class Widget { companion object : Factory { override fun create() = \"OK\" } }\n\
             fun use(): String = Widget.create()",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn classifier_value_call_keeps_inferred_generic_member_return() {
    let (errors, _) = check(
        "class C {\n\
                 companion object { private fun <T> create() = C() }\n\
                 class Nested { val value = C.create<String>() }\n\
             }",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn bound_extension_reference_prefers_nearest_receiver() {
    let (errors, _) = check(
        "interface Base\n\
             interface Derived : Base\n\
             fun Base.parse(value: String): Any = value\n\
             fun Derived.parse(value: String): Any = value\n\
             fun consume(reference: (String) -> Any) {}\n\
             fun use(value: Derived) { consume(value::parse) }",
    );

    assert!(
        errors.is_empty(),
        "the receiver hierarchy rank must survive candidate publication: {errors:?}"
    );
}

#[test]
fn callable_reference_local_keeps_its_receiver_function_shape() {
    let (errors, _) = check(
        "class A\n\
             fun A?.value(): String = \"OK\"\n\
             fun use(): String {\n\
                 val reference = A?::value\n\
                 return null.reference()\n\
             }",
    );

    assert!(
        errors.is_empty(),
        "a callable-reference local must invoke through its stored exact signature: {errors:?}"
    );
}

#[test]
fn declared_receiver_function_shape_wins_over_raw_inner_constructor_reference() {
    let (errors, _) = check(
        "class Foo { inner class Bar }\n\
             fun use(foo: Foo): Foo.Bar {\n\
                 val constructor: Foo.() -> Foo.Bar = Foo::Bar\n\
                 return foo.constructor()\n\
             }",
    );

    assert!(
        errors.is_empty(),
        "the declared receiver-function adaptation must survive local binding: {errors:?}"
    );
}

#[test]
fn inferred_extension_property_is_available_to_unbound_callable_reference() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "class A(val length: Int)\n\
             val A.myLength get() = this.length\n\
             fun consume(reference: (A) -> Int): Int = reference(A(2))\n\
             fun use(): Int = consume(A::myLength)",
        &mut diagnostics,
    );
    let files = vec![file];
    let symbols = collect_signatures(&files, &mut diagnostics);

    assert_no_diags(&diagnostics);
    assert_eq!(
        symbols
            .ext_props
            .values()
            .flatten()
            .find(|property| property.getter_name == "getMyLength")
            .map(|property| property.ty),
        Some(Ty::obj("kotlin/Int")),
        "signature inference must publish the extension property before reference selection"
    );
}

#[test]
fn missing_classifier_callable_reference_does_not_retry_classifier_as_value() {
    let (errors, _) = check(
        "class A\n\
             fun consume(reference: (A) -> Int): Int = 1\n\
             fun use(): Int = consume(A::missing)",
    );

    assert_eq!(errors, ["unresolved reference 'missing'."]);
}

#[test]
fn classifier_callable_reference_selects_companion_member_without_value_retry() {
    let (errors, _) = check(
        "class A { companion object { fun value(): String = \"OK\" } }\n\
             fun consume(reference: () -> String): String = reference()\n\
             fun use(): String = consume(A::value)",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn classifier_callable_reference_adapts_an_omitted_vararg() {
    let source = "import test.Factory\n\
             class Config\n\
             fun consume(reference: (Config) -> Boolean): Boolean = reference(Config())\n\
             fun use(): Boolean = consume(Factory::exists)";
    let (_, info, diagnostics) = retained_platform_analysis(source, Box::new(FakeMemberPlatform));

    assert!(diagnostics.diags.is_empty(), "{:#?}", diagnostics.diags);
    assert!(info.expr_lowers.values().any(|lowering| matches!(
            lowering,
            ExprLowering::AdaptedCallableReference {
                target: CallableReferenceTarget::Classifier(member),
                argument_mapping,
                ..
            } if member.name == "exists"
                && matches!(
                    argument_mapping.as_slice(),
                    [AdaptedRefArgument::Value(0), AdaptedRefArgument::Vararg { values, whole_array: false }]
                        if values.is_empty()
                )
        )));
}

#[test]
fn callable_shape_disambiguates_outer_and_companion_receivers() {
    let (errors, info) = check(
        "open class A { fun value(): Boolean = true; companion object : A() }\n\
             fun bound(reference: () -> Boolean): Boolean = reference()\n\
             fun unbound(reference: (A) -> Boolean): Boolean = reference(A.Companion)\n\
             fun use(): Boolean {\n\
                 val reference = A::value\n\
                 return bound(A::value) && unbound(reference)\n\
             }",
    );

    assert!(errors.is_empty(), "{errors:?}");
    let info = info.expect("checker result");
    assert!(info.expr_lowers.values().any(|lowering| matches!(
        lowering,
        ExprLowering::CallableReference {
            binding: CallableReferenceBinding::Singleton(singleton),
            target: CallableReferenceTarget::Member { receiver, member },
        } if singleton.classifier.nested_segment_ref() == "Companion"
            && *receiver == Ty::obj_name(singleton.classifier)
            && member.name == "value"
    )));
}

#[test]
fn equality_does_not_force_an_independent_generic_constructor_to_the_left_type() {
    let (errors, _) = check(
        "class Box<out T>(val value: T)\n\
             fun use(): Boolean = Box(1) != Box(\"x\")",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn enum_static_callable_reference_uses_synthesized_class_model_declaration() {
    let (errors, info) = check(
        "enum class E { ENTRY }\n\
             fun consume(reference: (String) -> E): E = reference(\"ENTRY\")\n\
             fun use(): E = consume(E::valueOf)",
    );

    assert!(errors.is_empty(), "{errors:?}");
    let info = info.expect("checker result");
    assert!(
        info.expr_lowers.values().any(|lowering| matches!(
            lowering,
            ExprLowering::CallableReference {
                target: CallableReferenceTarget::Classifier(member),
                ..
            } if member.name == "valueOf"
                && member.implicit_classifier_callable
                    == Some(crate::libraries::ImplicitClassifierCallable::EnumValueOf)
        )),
        "enum references must use the classifier model's selected callable"
    );
}

#[test]
fn concrete_enum_calls_are_selected_classifier_callables_not_reified_intrinsics() {
    let (errors, info) = check(
        "enum class E { FIRST, SECOND }\n\
             fun use(): E {\n\
                 val all = E.values()\n\
                 if (all.size == 0) return E.FIRST\n\
                 return E.valueOf(\"SECOND\")\n\
             }",
    );

    assert!(errors.is_empty(), "{errors:?}");
    let info = info.expect("checker result");
    let selected = info
        .resolved_calls
        .values()
        .filter_map(|call| match call {
            ResolvedCall::Companion(member) => Some(member.name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(selected.contains(&"values"), "selected calls: {selected:?}");
    assert!(
        selected.contains(&"valueOf"),
        "selected calls: {selected:?}"
    );
}

#[test]
fn fun_interface_constructor_reference_derives_shape_without_expected_type() {
    let (errors, _) = check(
        "fun interface KRunnable { fun run() }\n\
             val kr = ::KRunnable",
    );

    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn nested_constructor_references_resolve_from_type_and_bound_outer() {
    let mut diagnostics = DiagSink::new();
    let source = "class Command {\n\
                 class Add(val arg: String? = \"OK\")\n\
                 inner class InnerAdd(val arg: String? = \"OK\")\n\
             }\n\
             fun execute(func: () -> Command.Add) = func()\n\
             fun execute(c: Command, func: (Command) -> Command.InnerAdd) = func(c)\n\
             class Outer { inner class Inner(value: Int, vararg rest: String) { val result = value } }\n\
             fun <T> use0(f: (Int) -> T): T = f(1)\n\
             fun use(o: Outer) {\n\
                 execute(Command::Add); execute(Command(), Command::InnerAdd)\n\
                 use0(o::Inner).result\n\
             }";
    let _ = crate::frontend::analyze_source(
        source,
        jvm_platform(Box::new(FakeMemberPlatform)),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:#?}", diagnostics.diags);
}

#[test]
fn contextual_callable_refs_cover_generic_vararg_and_imported_object_members() {
    let mut diagnostics = DiagSink::new();
    let source = "import Host.hostFoo\n\
             fun <T> consume(block: (Any, T) -> String): String = \"\"\n\
             fun of(vararg values: Any): String = \"\"\n\
             fun withO(block: (String) -> String): String = \"\"\n\
             object Host { fun hostFoo(vararg values: String): String = \"\" }\n\
             fun use() { consume(::of); withO(::hostFoo) }";
    let (file, symbols, info) = crate::frontend::analyze_source(
        source,
        jvm_platform(Box::new(FakeMemberPlatform)),
        &mut diagnostics,
    );
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    let _ = info.expect("production frontend must check the source");
    let files = vec![file];
    {
        let scope = CheckerScope::root();
        let mut probe_diagnostics = DiagSink::new();
        let mut checker =
            make_checker(&files[0], 0, Some(&files), &symbols, &mut probe_diagnostics);
        for (name, outer) in [("of", "consume"), ("hostFoo", "withO")] {
            let reference = files[0]
                .expr_arena
                .iter()
                .enumerate()
                .find_map(|(index, expression)| match expression {
                    Expr::CallableRef {
                        receiver: None,
                        name: candidate,
                    } if candidate == name => Some(ExprId(index as u32)),
                    _ => None,
                })
                .expect("callable reference must be parsed");
            let expected = symbols
                .single_fun(outer)
                .and_then(|signature| signature.params.first().copied())
                .expect("outer function must expose its callable parameter");
            let resolved = checker.callable_ref_function_type(
                &scope,
                reference,
                Some(expected),
                None,
                name.to_string(),
            );
            assert!(
                    resolved.is_fun(),
                    "direct contextual resolution failed for ::{name}: expected={expected:?}, diagnostics={:#?}",
                    probe_diagnostics.diags
                );
        }
    }
    assert_no_diags(&diagnostics);
}

#[test]
fn all_bound_inner_constructor_vararg_shapes_infer_the_result() {
    let mut diagnostics = DiagSink::new();
    let source = "class Outer(val prefix: String) {\n\
                 inner class Inner1(value: Int, vararg rest: String) {\n\
                     val result = \"I1\" + prefix + value + if (rest.size == 0) \"E\" else rest[0]\n\
                 }\n\
                 inner class Inner2(value: Int, vararg rest: String = arrayOf(\"A\")) {\n\
                     val result = \"I2\" + prefix + value + rest[0]\n\
                 }\n\
             }\n\
             fun <T> use0(f: (Int) -> T): T = f(1)\n\
             fun <T> use1(f: (Int, String) -> T): T = f(1, \"x\")\n\
             fun use(o: Outer) {\n\
                 use0(o::Inner1).result; use1(o::Inner1).result\n\
                 use0(o::Inner2).result; use1(o::Inner2).result\n\
             }";
    let _ = crate::frontend::analyze_source(
        source,
        jvm_platform(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(crate::toolchain::jvm_classpath_jars_for(
                    "// WITH_STDLIB",
                )),
            ))
            .expect("JVM provider initialization"),
        )),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:#?}", diagnostics.diags);
}

#[test]
fn constructor_refs_are_not_shadowed_by_same_named_classpath_functions() {
    let source = "class Foo(val i: Int)\nval ref = ::Foo";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let constructor_ref = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::CallableRef {
                receiver: None,
                name,
            } if name == "Foo" => Some(ExprId(idx as u32)),
            _ => None,
        })
        .expect("source should contain ::Foo");
    assert_eq!(
        info.expr_types[constructor_ref.0 as usize],
        Ty::fun(vec![Ty::Int], Ty::obj("Foo"))
    );
    assert!(
        !matches!(
            info.expr_lowers.get(&constructor_ref),
            Some(ExprLowering::TopLevelFunctionRef(_))
        ),
        "constructor refs must not record a classpath top-level callable"
    );
}

#[test]
fn delegated_properties_record_classpath_extension_getvalue_for_lowering() {
    let source = "package test\nclass Delegate\nval prop: String by Delegate()";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let delegate_expr = file
        .decls
        .iter()
        .find_map(|decl| match file.decl(*decl) {
            Decl::Property(prop) if prop.name == "prop" => prop.delegate,
            _ => None,
        })
        .expect("source should contain delegated property");
    let target = info
        .delegate_getvalue(delegate_expr)
        .expect("checker must record delegated getValue target for lowering");
    let DelegateGetValueTarget::Extension { callable, .. } = target else {
        panic!("expected classpath extension getValue target, got {target:?}");
    };
    assert!(callable.owner.matches("test/DelegateKt"));
    assert_eq!(callable.name, "getValue");
    assert_eq!(callable.ret, Ty::String);
    assert!(
        info.synthetic_ext(delegate_expr, "getValue").is_none(),
        "delegated getValue must use the dedicated target map"
    );
}

#[test]
fn unannotated_delegated_properties_infer_classpath_extension_getvalue_return() {
    let source = "package test\nclass Delegate\nval prop by Delegate()";
    let mut diagnostics = DiagSink::new();
    let (file, symbols, info) = crate::frontend::analyze_source(
        source,
        jvm_platform(Box::new(FakeMemberPlatform)),
        &mut diagnostics,
    );
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    let info = info.expect("production frontend must check the source");
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        symbols.props.get("prop").map(|(ty, _, _)| *ty),
        Some(Ty::String)
    );

    let delegate_expr = file
        .decls
        .iter()
        .find_map(|decl| match file.decl(*decl) {
            Decl::Property(prop) if prop.name == "prop" => prop.delegate,
            _ => None,
        })
        .expect("source should contain delegated property");
    assert_eq!(
        info.delegate_getvalue(delegate_expr)
            .expect("checker must record delegated getValue")
            .ret(),
        Ty::String
    );
}

#[test]
fn local_delegated_properties_infer_classpath_extension_getvalue_return() {
    let source =
        "package test\nclass Delegate\nfun box(): String { val prop by Delegate(); return prop }";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );

    let delegate_expr = file
        .stmt_arena
        .iter()
        .find_map(|stmt| match stmt {
            Stmt::LocalDelegate { name, delegate, .. } if name == "prop" => Some(*delegate),
            _ => None,
        })
        .expect("source should contain local delegated property");
    assert_eq!(
        info.delegate_getvalue(delegate_expr)
            .expect("checker must record local delegated getValue")
            .ret(),
        Ty::String
    );
}

#[test]
fn classpath_member_slot_ties_do_not_fall_back_to_first_match() {
    let source = "fun ambiguous(s: String): String = s.tie(a = 1)";
    let (file, info, diagnostics) =
        retained_platform_analysis(source, Box::new(FakeMemberPlatform));
    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    assert_eq!(
        diagnostics.diags[0].msg,
        "overload resolution ambiguity between candidates:\n\
             fun tie(a: Long): String\n\
             fun tie(a: Byte): String"
    );
    assert_eq!(
        &source[diagnostics.diags[0].span.lo as usize..diagnostics.diags[0].span.hi as usize],
        "tie"
    );

    let call = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(idx, expr)| match expr {
            Expr::Call { callee, .. } => match file.expr(*callee) {
                Expr::Member { name, .. } if name == "tie" => Some(ExprId(idx as u32)),
                _ => None,
            },
            _ => None,
        })
        .expect("source should contain s.tie() call");
    assert!(
        !matches!(
            info.resolved_calls.get(&call),
            Some(ResolvedCall::Member(_))
        ),
        "ambiguous slot-aware classpath member call must not fall back to first-match resolution"
    );
}

fn err_contains(src: &str, needle: &str) {
    let (errs, _) = check(src);
    assert!(
        errs.iter().any(|e| e.contains(needle)),
        "expected error containing {needle:?}, got {errs:?}"
    );
}

fn err_exact(src: &str, expected: &str) {
    let (errors, _) = check(src);
    assert_eq!(errors, [expected]);
}

// NOTE: `require`/`check`/`error`/`TODO`/`assertEquals`/`assertTrue`/`assertFalse` are no longer
// hardcoded in the checker — they resolve generically from the classpath (a real stdlib / kotlin.test
// jar) and are validated by the box-conformance + `feature_box_e2e` suites, not here (these unit
// tests use `EmptySymbolSource`, so a classpath-resolved call can't be typed).

#[test]
fn private_member_property_access_is_checked() {
    // A `private` member property is readable only from within its declaring class — kotlinc rejects
    // a read from anywhere else, and so must krusty (rather than silently compiling it).
    err_contains(
        "class C { private val secret = 42 }\nfun box(): String = C().secret.toString()",
        "it is private in 'C'",
    );
    // Legal accesses from WITHIN the class (implicit `this`, explicit `this`, and another instance of
    // the same class) are accepted.
    ok("class C {\n  private val secret = 42\n  fun a(): Int = secret\n  fun b(): Int = this.secret\n  fun c(o: C): Int = o.secret\n}");
    // A class NESTED inside the owner (here an `inner class`) may read the enclosing private property
    // through an instance of the owner — kotlinc allows it, so the access check must not reject it.
    ok("class C {\n  private val secret = 42\n  inner class N {\n    fun r(o: C): Int = o.secret\n  }\n}");
}

#[test]
fn private_member_function_access_is_checked() {
    // A `private` member FUNCTION is callable only from within its class — a qualified call from
    // outside is rejected (kotlinc); a same-class call is accepted.
    err_contains(
            "class C { private fun secret(): Int = 1 }\nfun box(): String { C().secret(); return \"OK\" }",
            "it is private in 'C'",
        );
    ok("class C {\n  private fun secret(): Int = 1\n  fun via(o: C): Int = o.secret()\n}");
    ok(
        "class C { private fun choose(value: Int) = value; fun choose(value: String) = value }\n\
             fun box(): String = C().choose(\"OK\")",
    );
    err_contains(
        "class C { private fun choose(value: Int) = value; fun choose(value: String) = value }\n\
             fun box(): Int = C().choose(1)",
        "it is private in 'C'",
    );
}

#[test]
fn private_companion_member_reaches_the_containing_class_body() {
    // A `companion object`'s `private` members are in scope throughout the CONTAINING class's
    // body — including its nested classes, at any depth. kotlinc accepts all of these (it routes
    // the call through a synthetic `access$…` bridge); krusty widens the companion method to
    // public instead, so the only question here is the front-end check.
    ok("class C {\n  companion object { private fun make(): Int = 1 }\n  fun f(): Int = make()\n}");
    ok("class C {\n  companion object { private fun make(): Int = 1 }\n  class ZZZ { fun f(): Int = C.make() }\n}");
    ok("class C {\n  companion object { private fun make(): Int = 1 }\n  class ZZZ { class Deep { fun f(): Int = C.make() } }\n}");
    // The widening is the COMPANION's alone, and reaches only the containing class. A sibling
    // nested class's private member stays private to itself (kotlinc rejects both directions)…
    err_exact(
            "class C {\n  class Inner { private fun m(): Int = 1 }\n  class ZZZ { fun f(x: C.Inner): Int = x.m() }\n}",
            "cannot access 'fun m(): Int': it is private in 'C.Inner'.",
        );
    err_exact(
            "class C {\n  class Inner { private fun m(): Int = 1 }\n  companion object { fun f(x: C.Inner): Int = x.m() }\n}",
            "cannot access 'fun m(): Int': it is private in 'C.Inner'.",
        );
    // …and an unrelated top-level class still cannot reach the companion's private member.
    err_contains(
            "class C { companion object { private fun m(): Int = 1 } }\nclass D { fun f(): Int = C.m() }",
            "it is private in 'C$Companion'",
        );
}

#[test]
fn protected_member_property_access_is_checked() {
    // A `protected` member is unreadable from unrelated code (here a top-level function)…
    err_contains(
            "open class Base { protected val secret = 1 }\nfun box(): String = Base().secret.toString()",
            "it is protected in 'Base'",
        );
    // …but readable from a SUBCLASS (through a base-typed receiver inside the subclass).
    ok("open class Base { protected val secret = 1 }\nclass Sub : Base() {\n  fun r(b: Base): Int = b.secret\n}");
}

#[test]
fn containing_class_reaches_a_protected_member_inherited_by_its_companion() {
    ok("open class Base { protected fun secret(): Int = 1 }\n\
            class Container {\n\
                companion object : Base()\n\
                fun read(): Int = secret()\n\
            }");
}

#[test]
fn protected_super_companion_property_access_obeys_language_feature() {
    let source = "open class Base { companion object { protected const val secret: Int = 1 } }\n\
            class Derived : Base() { fun read(): Int = secret }";
    ok(source);

    let errors = check_with_detected_features(&format!(
        "// LANGUAGE: -AllowAccessToProtectedFieldFromSuperCompanion\n{source}"
    ));
    assert_eq!(errors, ["unresolved reference 'secret'."]);
}

#[test]
fn object_self_reference_resolves() {
    // An `object` may refer to itself by name inside its own body (it resolves to the singleton).
    // Previously this errored "unresolved reference". (Prerequisite for synthesized serializers,
    // whose `$serializer` object references its own INSTANCE.)
    ok("object Baz { fun me(): Baz = Baz }");
    // And an object used as a plain value elsewhere.
    ok("object Conf { val n: Int = 1 }\nfun f(): Conf = Conf");
}

#[test]
fn rejects_latent_miscompiles() {
    // Same-scope redeclaration is rejected (kotlinc errors too); legal nested-scope shadowing
    // (`var x` inside a block) is accepted — each declaration gets its own slot.
    err_contains(
        "fun box(): String { var x = 1; var x = 2; return \"OK\" }",
        "conflicting local declaration",
    );
    ok("fun box(): String { var x = 1; if (1>0) { var x = 2; x.toString() }; return \"OK\" }");
    // Production streaming preserves source initialization order, so the legacy AST emitter's
    // old capability rejection must not leak into the production frontend.
    ok(
            "class Foo(v: Int) { init { set(v) }\n fun set(x: Int) { field = x }\n var field: Int = 0 }\nfun box(): String = \"OK\"",
        );
    // Value-class representation is backend-owned. `Int` and `UInt` remain distinct semantic
    // overload parameters throughout frontend selection even if one target later chooses
    // colliding physical carriers.
    ok("fun f(x: Int): Int = x\nfun f(x: UInt): UInt = x\nfun box(): String = \"OK\"");
}

#[test]
fn named_arguments() {
    // Accepted: named (any order), named combined with an omitted default, and named arguments on a
    // same-file class MEMBER (reordering is realized by the lowerer's source-order temp spill).
    ok("fun f(a: Int, b: Int): Int = a - b\nfun g(): Int = f(b = 2, a = 5)");
    ok("fun f(a: Int, b: Int = 10): Int = a + b\nfun g(): Int = f(a = 1)");
    ok("class C { fun m(a: Int, b: Int): Int = a - b }\nfun g(): Int = C().m(b = 2, a = 5)");
    ok(
        "fun f(a: String, b: String, c: () -> String): String = a + b + c()\n\
            fun g(): String {\n\
                var order = \"\"\n\
                return f(\n\
                    b = { order += \"b\"; \"B\" }(),\n\
                    a = { order += \"a\"; \"A\" }(),\n\
                    c = { order += \"c\"; \"C\" },\n\
                )\n\
            }",
    );
    // Rejected: unknown parameter name.
    err_contains(
        "fun f(a: Int): Int = a\nfun g(): Int = f(z = 1)",
        "no parameter with name 'z' found.",
    );
    err_contains(
        "fun f(a: Int): Int = a\nfun g(): Int = f(a = 1, a = 2)",
        "argument already passed for this parameter.",
    );
}

#[test]
fn call_argument_mapping_collects_structural_and_missing_errors() {
    let args = [ExprId(0), ExprId(1), ExprId(2)];
    let names = [Some("third".to_string()), None, Some("unknown".to_string())];
    let params = ["first", "second", "third"].map(str::to_string);

    let failure = map_call_args(
        &args,
        Some(&names),
        &params,
        params.len(),
        3,
        &[],
        None,
        false,
    )
    .expect_err("reordered named argument followed by a positional argument");

    assert_eq!(
        failure.errors,
        vec![
            CallArgMappingError::PositionalAfterNamed { argument: 1 },
            CallArgMappingError::NoParameterNamed {
                name: "unknown".to_string(),
                argument: 2,
            },
        ]
    );
}

#[test]
fn call_argument_mapping_recovery_requires_overload_unanimity() {
    let duplicate = CallArgMappingFailure {
        errors: vec![CallArgMappingError::AlreadyPassed { argument: 1 }],
        recovery_argument: None,
    };
    let unknown = CallArgMappingFailure {
        errors: vec![CallArgMappingError::NoParameterNamed {
            name: "a".to_string(),
            argument: 0,
        }],
        recovery_argument: None,
    };

    for mut conflicts in [
        vec![(duplicate.clone(), 0), (unknown.clone(), 1)],
        vec![(unknown.clone(), 0), (duplicate.clone(), 1)],
    ] {
        assert_eq!(take_unanimous_mapping_error(&mut conflicts), None);
    }

    let mut unanimous = vec![(duplicate.clone(), 0), (duplicate.clone(), 1)];
    assert_eq!(
        take_unanimous_mapping_error(&mut unanimous).map(|(failure, _)| failure),
        Some(duplicate)
    );
}

#[test]
fn call_argument_mapping_accepts_matching_positional_order() {
    let args = [ExprId(0), ExprId(1)];
    let names = [Some("first".to_string()), None];
    let params = ["first", "second"].map(str::to_string);

    assert_eq!(
        map_call_args(
            &args,
            Some(&names),
            &params,
            params.len(),
            2,
            &[],
            None,
            false,
        ),
        Ok(vec![Some(args[0]), Some(args[1])])
    );
}

#[test]
fn call_argument_mapping_rejects_trailing_lambda_for_final_vararg() {
    let args = [ExprId(0), ExprId(1)];
    let params = ["prefix", "actions"].map(str::to_string);

    let failure = map_call_args(&args, None, &params, params.len(), 2, &[], Some(1), true)
        .expect_err("a trailing lambda cannot supply a final vararg");

    assert_eq!(
        failure.errors,
        vec![CallArgMappingError::TrailingLambdaOnVararg { argument: 1 }]
    );
}

#[test]
fn call_argument_mapping_treats_non_final_vararg_as_optional() {
    let args = [ExprId(0)];
    let names = [Some("tail".to_string())];
    let params = ["values", "tail"].map(str::to_string);

    assert_eq!(
        map_call_args(
            &args,
            Some(&names),
            &params,
            params.len(),
            2,
            &[],
            Some(0),
            false,
        ),
        Ok(vec![None, Some(args[0])])
    );
}

#[test]
fn arithmetic_ok() {
    ok("fun f(a: Int, b: Int): Int = a + b * 2");
    ok("fun f(a: Double, b: Int): Double = a + b"); // promotion Int->Double
    ok("fun f(a: Long, b: Int): Long = a * b");
}

#[test]
fn string_concat() {
    ok("fun f(a: Int, b: String): String = a.toString() + b");
    ok("fun f(a: Int): String = \"x=\" + a"); // Int+String via concat
}

#[test]
fn comparison_and_logic() {
    ok("fun f(a: Int, b: Int): Boolean = a < b && a != b");
}

#[test]
fn if_branches_common_type() {
    ok("fun max(a: Int, b: Int): Int = if (a > b) a else b");
    err_contains(
        "fun f(a: Int, b: String): Int = if (a > 0) a else b",
        "return type mismatch: expected 'Int', actual 'Any'.",
    );
}

#[test]
fn return_type_mismatch() {
    err_contains(
        "fun f(a: Int): String = a",
        "return type mismatch: expected 'String', actual 'Int'.",
    );
}

#[test]
fn unresolved_reference() {
    err_contains("fun f(): Int = q", "unresolved reference 'q'.");
}

#[test]
fn subclass_constructor_prefers_nested_type_from_superclass_to_package_type() {
    let mut diagnostics = DiagSink::new();
    let source = "class Category\n\
             open class Parent { enum class Category { FIRST } }\n\
             class Child(category: Category) : Parent()";
    let (_, symbols, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");

    assert_no_diags(&diagnostics);
    assert_eq!(
        symbols
            .classes
            .get(&type_name("Child"))
            .expect("subclass signature")
            .ctor_params,
        [Ty::obj("Parent$Category")]
    );
}

#[test]
fn subclass_resolves_nested_type_through_superclass_chain() {
    ok("open class Root { class Category }\n\
            open class Parent : Root()\n\
            class Child(category: Category) : Parent()");
}

#[test]
fn subclass_infers_property_type_from_inherited_nested_classifier() {
    let mut diagnostics = DiagSink::new();
    let source = "open class Parent { enum class Category { FIRST } }\n\
             class Child : Parent() { val category = Category.FIRST }";
    let (_, symbols, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");

    assert_no_diags(&diagnostics);
    assert_eq!(
        symbols
            .classes
            .get(&type_name("Child"))
            .and_then(|class| class.declared_props.get("category"))
            .map(|property| property.ty),
        Some(Ty::obj("Parent$Category"))
    );
}

#[test]
fn subclass_inference_is_independent_of_superclass_declaration_order() {
    let mut diagnostics = DiagSink::new();
    let source = "class Child : Parent() { val category = Category.FIRST }\n\
             open class Parent { enum class Category { FIRST } }";
    let (_, symbols, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");

    assert_no_diags(&diagnostics);
    assert_eq!(
        symbols
            .classes
            .get(&type_name("Child"))
            .and_then(|class| class.declared_props.get("category"))
            .map(|property| property.ty),
        Some(Ty::obj("Parent$Category"))
    );
}

#[test]
fn private_inherited_nested_classifier_does_not_shadow_top_level_type() {
    let mut diagnostics = DiagSink::new();
    let source = "class Category\n\
             open class Parent { private class Category }\n\
             class Child(category: Category) : Parent()";
    let (_, symbols, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");

    assert_no_diags(&diagnostics);
    assert_eq!(
        symbols
            .classes
            .get(&type_name("Child"))
            .expect("subclass signature")
            .ctor_params,
        [Ty::obj("Category")]
    );
}

#[test]
fn protected_inherited_nested_classifier_is_visible_to_subclass() {
    ok("open class Parent { protected class Category }\n\
            class Child(category: Category) : Parent()");
}

#[test]
fn protected_nested_classifier_is_visible_in_its_declaring_class_signature() {
    ok("open class Parent {\n\
                protected class Stage\n\
                protected class Builder\n\
                protected fun build(block: Builder.() -> Unit): Builder = Builder()\n\
            }\n\
            class Child : Parent() { fun use() = build {} }");
}

#[test]
fn nested_classifier_body_can_access_protected_sibling_of_lexical_owner() {
    ok("open class Outer {\n\
                protected class Stage(val run: () -> Unit)\n\
                protected class Builder {\n\
                    fun stage(block: () -> Unit): Stage = Stage(block)\n\
                }\n\
            }");
}

#[test]
fn subclass_does_not_choose_between_peer_inherited_nested_types() {
    let left = type_name("Left");
    let right = type_name("Right");
    assert_eq!(
        crate::symbol_resolver::inherited_nested_classifier_name(
            "Category",
            vec![left, right],
            |_| Vec::new(),
            |candidate| {
                candidate == type_name("Left$Category") || candidate == type_name("Right$Category")
            },
        ),
        InheritedNestedClassifier::Ambiguous
    );
}

#[test]
fn val_reassign_is_error() {
    err_contains(
        "fun f(): Int {\n val x = 1\n x = 2\n return x\n}",
        "cannot be reassigned",
    );
}

#[test]
fn deferred_val_write_through_explicit_or_narrowed_this_is_constructor_initialization() {
    ok("class Secondary {\n\
                val value: String\n\
                constructor(value: String) { this.value = value }\n\
            }\n\
            open class Base {\n\
                val value: Any?\n\
                init {\n\
                    this as Derived\n\
                    this.value = \"OK\"\n\
                }\n\
            }\n\
            class Derived : Base()");
}

#[test]
fn val_reassignment_diagnostic_points_at_assignment_target() {
    let source = "fun reassignValue(): Int {\n  val value = 1\n  value = 2\n  return value\n}";
    let (_, _, diagnostics) = retained_standalone_analysis(source);

    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    let diagnostic = &diagnostics.diags[0];
    assert_eq!(diagnostic.msg, "'val' cannot be reassigned.");
    assert_eq!(
        &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
        "value"
    );
}

#[test]
fn var_reassign_ok() {
    ok("fun f(): Int {\n var x = 1\n x = 2\n return x\n}");
}

#[test]
fn call_arity_and_types() {
    ok("fun a(x: Int): Int = x\nfun b(): Int = a(1)");
    err_contains(
        "fun a(x: Int): Int = x\nfun b(): Int = a()",
        "no value passed for parameter 'x'.",
    );
    err_contains(
        "fun a(x: Int): Int = x\nfun b(): Int = a(\"s\")",
        "argument type mismatch: actual type is 'String', but 'Int' was expected.",
    );
}

#[test]
fn missing_required_argument_diagnostic_points_at_callee() {
    let source = "fun pair(left: Int, right: String): Int = left\n\
                      fun missingArgument(): Int = pair(1)";
    let (_, _, diagnostics) = retained_standalone_analysis(source);

    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    let diagnostic = &diagnostics.diags[0];
    assert_eq!(diagnostic.msg, "no value passed for parameter 'right'.");
    assert_eq!(
        &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
        missing_argument_anchor("1", "pair")
    );
    let editor_span = diagnostic.editor_span.expect("official editor range");
    assert_eq!(
        &source[editor_span.lo as usize..editor_span.hi as usize],
        "pair"
    );
}

#[test]
fn missing_named_argument_diagnostic_points_at_callee() {
    let source = "fun namedPair(left: Int, right: String): Int = left\n\
                      fun missingNamedArgument(): Int = namedPair(left = 1)";
    let (_, _, diagnostics) = retained_standalone_analysis(source);

    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    let diagnostic = &diagnostics.diags[0];
    assert_eq!(diagnostic.msg, "no value passed for parameter 'right'.");
    assert_eq!(
        &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
        missing_argument_anchor("left", "namedPair")
    );
    let editor_span = diagnostic.editor_span.expect("official editor range");
    assert_eq!(
        &source[editor_span.lo as usize..editor_span.hi as usize],
        "namedPair"
    );
}

#[test]
fn block_while_fib_typechecks() {
    ok("fun fib(n: Int): Int {\n var a = 0\n var b = 1\n var i = 0\n while (i < n) {\n   val t = a + b\n   a = b\n   b = t\n   i = i + 1\n }\n return a\n}");
}

#[test]
fn bool_operator_misuse() {
    let (errors, _) = check("fun f(a: Int): Boolean = a && a");
    assert_eq!(
        errors,
        [
            "condition type mismatch: inferred type is 'Int' but 'Boolean' was expected.",
            "condition type mismatch: inferred type is 'Int' but 'Boolean' was expected.",
        ]
    );
}

#[test]
fn string_instance_methods() {
    ok("fun f(s: String): String = s.substring(1)");
    ok("fun f(s: String): String = s.substring(1, 3)");
    ok("fun f(s: String): Int = s.indexOf(\"x\")");
    // (`concat` is a `java.lang.String` method with no Kotlin counterpart — kotlinc reports it as
    // unresolved, so the classpath-free fallback must not invent it either. Covered with the rest of
    // the Java-only member set in `mapped_string_scope_e2e`, which checks against a real stdlib.)
    err_contains(
        "fun f(s: String): String = s.substring(\"x\")",
        "argument type mismatch: actual type is 'String', but 'Int' was expected.",
    );
    let (errors, _) = check("fun f(a: Int): Int = a.substring(1)");
    assert_eq!(
        errors,
        [crate::diagnostic_wording::unresolved_reference_on(
            "substring",
            Some("Int")
        )]
    );
}

#[test]
fn nullable_receiver_calls_use_the_safe_call_diagnostic_and_dot_span() {
    let source = "class C { fun member(value: Int): Int = value }\n\
                      class Holder(val block: () -> Int)\n\
                      fun sourceMember(value: C?): Int = value.member(1)\n\
                      fun classpathMember(value: String?): String = value.substring(1)\n\
                      fun spacedClasspathMember(value: String?): String = value. /* gap */ substring(1)\n\
                      fun String.nonNullExtension(): Int = length\n\
                      fun sourceExtension(value: String?): Int = value.nonNullExtension()\n\
                      fun callableProperty(value: Holder?): Int = value.block()\n\
                      fun nullableInvoke(block: (() -> Int)?): Int = block.invoke()\n\
                      fun nullableEquals(value: Any?): Boolean = value.equals(null)";
    let (_, _, diagnostics) = retained_standalone_analysis(source);

    assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            vec![
                "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'C?'.",
                "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
                "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
                "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
                "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Holder?'.",
                "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type '(() -> Int)?'.",
                "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Any?'.",
            ]
        );
    for diagnostic in &diagnostics.diags {
        assert_eq!(
            &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize],
            "."
        );
    }
}

#[test]
fn nullable_receiver_extensions_safe_calls_assertions_and_smart_casts_remain_valid() {
    ok("fun String?.nullableExtension(): Int = this?.length ?: 0\n\
             fun valid(value: String?): Int {\n\
             \u{20}\u{20}val extension = value.nullableExtension()\n\
             \u{20}\u{20}val safe = value?.substring(1)\n\
             \u{20}\u{20}val asserted = value!!.substring(1)\n\
             \u{20}\u{20}if (value != null) return value.substring(1).length\n\
             \u{20}\u{20}return extension + (safe?.length ?: 0) + asserted.length\n\
             }");
    ok("class C { fun call(): Int = 1 }\n\
             fun C?.call(): Int = 2\n\
             fun choose(value: C?): Int = value.call()");
    ok(
            "inline fun <T> T.acceptsNullable(block: () -> Unit): Boolean { block(); return this == null }\n\
             fun valid(value: String?): Boolean = value.acceptsNullable { }",
        );
}

#[test]
fn explicit_non_null_generic_extension_rejects_nullable_receiver() {
    let source = "fun <T : Any> T.rejectsNullable(): Int = 1\n\
                      fun invalid(value: String?): Int = value.rejectsNullable()";
    let (_, _, diagnostics) = retained_standalone_analysis(source);

    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    assert_eq!(
            diagnostics.diags[0].msg,
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'."
        );
    assert_eq!(
        &source[diagnostics.diags[0].span.lo as usize..diagnostics.diags[0].span.hi as usize],
        "."
    );
}

#[test]
fn generic_extension_registry_filters_by_bound_and_has_stable_families() {
    let source = "open class Text\n\
                      class TextValue : Text()\n\
                      open class Numeric\n\
                      class NumericValue : Numeric()\n\
                      fun <T : Text> T.pick(): Int = 1\n\
                      fun <R : Text> R.pick(value: Int): Int = value\n\
                      fun <N : Numeric> N.pick(value: Int, other: Int): Int = value + other";
    let mut diagnostics = DiagSink::new();
    let file = parse_file(source, &mut diagnostics);
    let symbols = collect_signatures(&[file], &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let string_overloads = symbols.ext_fun_overloads(Ty::obj("TextValue"), "pick");
    assert_eq!(
        string_overloads
            .iter()
            .map(|signature| signature.params.len())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let int_overloads = symbols.ext_fun_overloads(Ty::obj("NumericValue"), "pick");
    assert_eq!(
        int_overloads
            .iter()
            .map(|signature| signature.params.len())
            .collect::<Vec<_>>(),
        vec![2]
    );
}

#[test]
fn exact_extension_family_keeps_applicable_concrete_supertype_overloads() {
    let source = "open class Base\n\
                      class Derived : Base()\n\
                      fun Derived.pick(): Int = 1\n\
                      fun Base.pick(value: Int): Int = value";
    let mut diagnostics = DiagSink::new();
    let file = parse_file(source, &mut diagnostics);
    let symbols = collect_signatures(&[file], &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    assert_eq!(
        symbols
            .ext_fun_overloads(Ty::obj("Derived"), "pick")
            .iter()
            .map(|signature| signature.params.len())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn reference_types_resolve() {
    // class-typed param + property read + construction + instance call all typecheck.
    ok("class Point(val x: Int, val y: Int)\nfun ox(p: Point): Int = p.x");
    ok("class Point(val x: Int)\nfun mk(): Point = Point(3)");
    ok("class Point(val x: Int) {\n  fun get(): Int = x\n}\nfun use(p: Point): Int = p.get()");
    ok("class Box(val v: Int)\nclass Pair(val a: Box, val b: Box)\nfun first(p: Pair): Int = p.a.v");
    // forward reference: a function can mention a class declared later.
    ok("fun ox(p: Point): Int = p.x\nclass Point(val x: Int)");
}

#[test]
fn bounded_star_projection_uses_the_source_classifier_bound() {
    let mut diagnostics = DiagSink::new();
    let source = "interface E\nclass Box<T : E>(val value: T)\nclass Wrapper(val box: Box<*>)";
    let (file, symbols, info) =
        crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    let info = info.expect("production frontend must check the source");
    assert_no_diags(&diagnostics);

    let source = crate::module_symbols::ModuleSymbols::new(&symbols);
    let classifier = crate::symbol_source::SymbolSource::classifier(&source, type_name("Box"))
        .expect("source Box classifier");
    assert_eq!(classifier.type_param_bounds, vec![vec![Ty::obj("E")]]);

    let property_type = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class) if class.name == "Wrapper" => Some(&class.props[0].ty),
            _ => None,
        })
        .expect("Wrapper.box type");
    assert_eq!(
        info.resolved_type(property_type),
        Some(Ty::obj_args("Box", &[Ty::star_projection(Ty::obj("E"))])),
    );
    assert_eq!(
        symbols
            .class_by_type_name(type_name("Wrapper"))
            .and_then(|class| class.declared_props.get("box"))
            .map(|property| property.ty),
        Some(Ty::obj_args("Box", &[Ty::star_projection(Ty::obj("E"))])),
        "the checked declaration must replace the collection pass's erased star bound",
    );
    let box_class = symbols
        .class_by_type_name(type_name("Box"))
        .expect("checked Box signature");
    let box_parameter = Ty::ty_param(
        box_class.type_parameters.type_params[0].as_str(),
        box_class.type_parameters.type_param_bounds[0],
    );
    assert_eq!(
        box_class.ctor_param_shapes[0].0, box_parameter,
        "the semantic constructor shape retains the class type parameter",
    );
    assert_eq!(
        box_class.generic_property_shapes.get("value").copied(),
        Some(box_parameter),
    );
    assert_eq!(
            box_class
                .declared_props
                .get("value")
                .map(|property| property.ty),
            Some(box_parameter),
            "the semantic property keeps T; only a backend may choose its physical bound representation",
        );
}

#[test]
fn reference_type_errors() {
    let (errors, _) = check("class Point(val x: Int)\nfun f(p: Point): Int = p.z");
    assert_eq!(
        errors,
        [crate::diagnostic_wording::unresolved_reference_on(
            "z",
            Some("Point")
        )]
    );
    err_contains(
        "class Point(val x: Int)\nfun f(): Point = Point()",
        "no value passed for parameter 'x'.",
    );
    err_contains(
        "fun f(p: TextOnly): Int = 0",
        "unresolved reference 'TextOnly'.",
    );
}

#[test]
fn source_extension_overloads_keep_semantically_distinct_shapes() {
    let assert_frontend_accepts = |source: &str| {
        let (messages, _) = check(source);
        assert!(messages.is_empty(), "{messages:?}");
    };
    assert_frontend_accepts(
        "class Box<T>\n\
             operator fun String.plusAssign(value: Box<String>) {}\n\
             operator fun String.plusAssign(value: Box<Int>) {}",
    );
    assert_frontend_accepts(
        "fun <T> Array<T>.nestedClash(): Int = 1\n\
             fun Array<Any?>.nestedClash(): Int = 2",
    );
    assert_frontend_accepts(
        "fun ((Int) -> Int).functionReceiverClash(): Int = 1\n\
             fun ((String) -> String).functionReceiverClash(): Int = 2",
    );
    assert_frontend_accepts(
        "fun Int.unsignedReceiverClash(): Int = 1\n\
             fun UInt.unsignedReceiverClash(): Int = 2",
    );
}

#[test]
fn nullable_inaccessible_callable_property_reports_access_once() {
    let source = "class C(private val block: () -> Int)\n\
                      fun invalid(value: C?): Int = value.block()";
    let (_, _, diagnostics) = retained_standalone_analysis(source);

    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    assert_eq!(
        diagnostics.diags[0].msg,
        "cannot access 'block': it is private in 'C'"
    );
}

#[test]
fn unary_and_inc_dec_expressions_record_exact_operator_targets() {
    let source = r#"
class Counter {
    operator fun unaryMinus(): Counter = this
    operator fun inc(): Counter = this
}
fun use(counter: Counter) {
    var current = counter
    val negative = -current
    current++
    ++current
    val previous = current++
    val next = ++current
}
"#;
    let (file, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let unary = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| {
            matches!(expression, Expr::Unary { op: UnOp::Neg, .. }).then_some(ExprId(index as u32))
        })
        .expect("user unary expression");
    assert!(matches!(
        info.resolved_operator_call(unary, "unaryMinus"),
        Some(ResolvedCall::Member(member))
            if matches!(member.origin, Origin::Module { .. })
                && member.member.name == "unaryMinus"
    ));

    let expression_inc_dec = file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, _)| {
            let expression = ExprId(index as u32);
            (matches!(file.expr(expression), Expr::IncDec { .. })
                && info.ty(expression) != Ty::Error)
                .then_some(expression)
        })
        .collect::<Vec<_>>();
    assert_eq!(expression_inc_dec.len(), 2);
    let expression_targets = expression_inc_dec
        .iter()
        .map(|&expression| {
            (
                expression,
                info.ty(expression),
                info.resolved_operator_call(expression, "inc").is_some(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        expression_targets.iter().all(|(_, _, resolved)| *resolved),
        "missing increment targets: {expression_targets:?}"
    );

    let statement_inc_dec = file
        .stmt_arena
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| {
            matches!(statement, Stmt::IncDec { .. }).then_some(StmtId(index as u32))
        })
        .collect::<Vec<_>>();
    assert_eq!(statement_inc_dec.len(), 2);
    assert!(statement_inc_dec
        .iter()
        .all(|&statement| info.resolved_stmt_operator_call(statement, "inc").is_some()));
}

#[test]
fn symbolic_operators_require_the_operator_modifier_at_exact_locations() {
    let source = "class Counter {\n\
                          fun unaryMinus(): Counter = this\n\
                          fun inc(): Counter = this\n\
                          fun plusAssign(value: Int) {}\n\
                          fun plus(other: Int): Counter = this\n\
                          fun plus(other: Counter): Counter = this\n\
                      }\n\
                      fun use(counter: Counter) {\n\
                          val explicit = counter.unaryMinus()\n\
                          counter.plusAssign(1)\n\
                          val explicitBinary = counter.plus(counter)\n\
                          val symbolic = -counter\n\
                          val binary = counter + counter\n\
                          var current = counter\n\
                          current++\n\
                          current += 1\n\
                      }";
    let (_, info, diagnostics) = retained_standalone_analysis(source);

    assert!(info
        .resolved_calls
        .values()
        .any(|call| matches!(call, ResolvedCall::Member(member)
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.name == "unaryMinus")));
    assert!(info
        .resolved_calls
        .values()
        .any(|call| matches!(call, ResolvedCall::Member(member)
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.name == "plus")));
    assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            [
                "'operator' modifier is required on 'fun unaryMinus(): Counter' defined in 'Counter'.",
                "'operator' modifier is required on 'fun plus(other: Counter): Counter' defined in 'Counter'.",
                "'operator' modifier is required on 'fun inc(): Counter' defined in 'Counter'.",
                "'operator' modifier is required on 'fun plus(other: Int): Counter' defined in 'Counter'.",
            ],
        );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| { &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize] })
            .collect::<Vec<_>>(),
        ["-", "+", "++", "+="],
    );
    assert!(!info
        .stmt_lowers
        .values()
        .any(|lowering| matches!(lowering, StmtLowering::PlusAssign(_))));
}

#[test]
fn overrides_inherit_operator_convention_from_the_base_signature() {
    let source = "open class BaseCounter {\n\
                          open operator fun unaryMinus(): BaseCounter = this\n\
                          open operator fun inc(): BaseCounter = this\n\
                          open operator fun plus(other: DerivedCounter): DerivedCounter = other\n\
                      }\n\
                      class DerivedCounter : BaseCounter() {\n\
                          override fun unaryMinus(): DerivedCounter = this\n\
                          override fun inc(): DerivedCounter = this\n\
                          override fun plus(other: DerivedCounter): DerivedCounter = this\n\
                      }\n\
                      fun use(counter: DerivedCounter): DerivedCounter {\n\
                          var current = counter\n\
                          val negative = -current\n\
                          val combined = current + current\n\
                          current++\n\
                          return negative\n\
                      }";
    let (_, info, diagnostics) = retained_standalone_analysis(source);
    assert_no_diags(&diagnostics);

    let targets = info
        .resolved_operator_calls
        .values()
        .chain(info.resolved_stmt_operator_calls.values())
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 3);
    assert!(targets.iter().all(|target| {
        matches!(
            target,
            ResolvedCall::Member(member)
                if matches!(member.origin, Origin::Module { .. })
                    && member.member.owner.is_some_and(|owner| owner.matches("DerivedCounter"))
        )
    }));
}

#[test]
fn interface_operator_convention_reaches_overrides_and_inherited_defaults() {
    let source = "interface OperatorBase {\n\
                          operator fun unaryMinus(): InterfaceCounter\n\
                          operator fun inc(): InterfaceCounter\n\
                      }\n\
                      class InterfaceCounter : OperatorBase {\n\
                          override fun unaryMinus(): InterfaceCounter = this\n\
                          override fun inc(): InterfaceCounter = this\n\
                      }\n\
                      interface DefaultOperators {\n\
                          operator fun unaryMinus(): DefaultCounter = DefaultCounter()\n\
                          operator fun inc(): DefaultCounter = DefaultCounter()\n\
                      }\n\
                      class DefaultCounter : DefaultOperators\n\
                      fun useOverride(counter: InterfaceCounter) {\n\
                          var current = counter\n\
                          val negative = -current\n\
                          current++\n\
                      }\n\
                      fun useDefault(counter: DefaultCounter) {\n\
                          var current = counter\n\
                          val negative = -current\n\
                          current++\n\
                      }";
    let mut diagnostics = DiagSink::new();
    let inputs = [crate::frontend::SourceInput::kotlin(source)];
    let mut analysis = crate::frontend::analyze_source_set_with_features_and_prepare_retained(
        &inputs,
        jvm_platform(Box::new(crate::libraries::EmptySymbolSource)),
        &LangFeatures::new(),
        |_, symbols| {
            // Keep interface search active when superclass metadata is external.
            symbols
                .classes
                .get_mut(&type_name("DefaultCounter"))
                .expect("default counter signature")
                .super_internal = Some(type_name("platform/ClasspathBase"));
        },
        &mut diagnostics,
    );
    let info = analysis
        .types
        .pop()
        .flatten()
        .expect("production frontend must check the source");
    assert_no_diags(&diagnostics);

    let mut owners = info
        .resolved_operator_calls
        .values()
        .chain(info.resolved_stmt_operator_calls.values())
        .filter_map(|target| match target {
            ResolvedCall::Member(member) if matches!(member.origin, Origin::Module { .. }) => {
                Some((member.member.owner?.render(), member.member.is_interface()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    owners.sort_unstable();
    assert_eq!(
        owners,
        [
            ("DefaultOperators".to_string(), true),
            ("DefaultOperators".to_string(), true),
            ("InterfaceCounter".to_string(), false),
            ("InterfaceCounter".to_string(), false),
        ]
    );
}

#[test]
fn private_member_operators_are_rejected_outside_the_declaring_class() {
    let source = "class PrivateCounter {\n\
                          private operator fun unaryMinus(): PrivateCounter = this\n\
                          private operator fun inc(): PrivateCounter = this\n\
                          fun inside(): PrivateCounter = -this\n\
                      }\n\
                      fun outside(counter: PrivateCounter) {\n\
                          val negative = -counter\n\
                          var current = counter\n\
                          current++\n\
                      }";
    let (_, _, diagnostics) = retained_standalone_analysis(source);
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        [
            "cannot access 'unaryMinus': it is private in 'PrivateCounter'",
            "cannot access 'inc': it is private in 'PrivateCounter'",
        ],
    );
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| { &source[diagnostic.span.lo as usize..diagnostic.span.hi as usize] })
            .collect::<Vec<_>>(),
        ["-counter", "current++"],
    );
}

#[test]
fn cross_file_private_operator_extension_is_not_a_candidate() {
    let sources = [
        "class Counter\nprivate operator fun Counter.unaryMinus(): Counter = this",
        "fun use(counter: Counter): Counter = -counter",
    ];
    let (files, types, diagnostics) = retained_standalone_source_set(&sources);
    let info = &types[1];

    let unary = files[1]
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| {
            matches!(expression, Expr::Unary { op: UnOp::Neg, .. }).then_some(ExprId(index as u32))
        })
        .expect("cross-file unary expression");
    assert!(info.resolved_operator_call(unary, "unaryMinus").is_none());
    assert_eq!(diagnostics.diags.len(), 1, "{:?}", diagnostics.diags);
    let error = &diagnostics.diags[0];
    assert_eq!(error.file, 1);
    assert_eq!(error.msg, "operator cannot be applied to 'Counter'");
    assert_eq!(
        &sources[1][error.span.lo as usize..error.span.hi as usize],
        "-counter"
    );
}

#[test]
fn source_extension_declarations_use_the_direct_index() {
    let sources = [
        "package sample\nfun String.first(value: Int): Int = value",
        "package sample\nfun String.second(): String = this",
    ];
    let mut diagnostics = DiagSink::new();
    let files = sources
        .iter()
        .map(|source| parse_file(source, &mut diagnostics))
        .collect::<Vec<_>>();
    let symbols = collect_signatures(&files, &mut diagnostics);
    assert_no_diags(&diagnostics);

    let first = files[0].decls[0];
    let second = files[1].decls[0];
    assert_eq!(
        symbols
            .source_extension_function(0, first)
            .map(|(_, signature)| signature.params.clone()),
        Some(vec![Ty::Int])
    );
    assert_eq!(
        symbols
            .source_extension_function(1, second)
            .map(|(receiver, signature)| (receiver, signature.ret)),
        Some((Ty::String, Ty::String))
    );
}

#[test]
fn cast_after_unary_keeps_builtin_dispatch() {
    let Some(jdk_modules) = crate::toolchain::jdk_modules() else {
        return;
    };
    let source = "data class A(val a: Int)\n\
             fun box(): String {\n\
                 val v1 = A(-10.toInt()).hashCode()\n\
                 val v2 = (-10.toInt() as Int?)!!.hashCode()\n\
                 return if (v1 == v2) \"OK\" else \"$v1 $v2\"\n\
             }";
    let mut classpath_entries = crate::toolchain::classpath_jars_for("");
    classpath_entries.push(jdk_modules);
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath_entries));
    let platform = initialized_jvm_libraries(classpath);
    let (file, info, diagnostics) = retained_platform_analysis(source, Box::new(platform));
    assert_no_diags(&diagnostics);

    let unary_expressions = file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            matches!(expression, Expr::Unary { op: UnOp::Neg, .. }).then_some(ExprId(index as u32))
        })
        .collect::<Vec<_>>();
    assert!(!unary_expressions.is_empty());
    for unary in unary_expressions {
        assert_eq!(info.ty(unary), Ty::Int);
        let operand = match file.expr(unary) {
            Expr::Unary { operand, .. } => *operand,
            _ => unreachable!(),
        };
        assert!(
            info.resolved_operator_call(unary, "unaryMinus").is_none(),
            "cast precedence must not select a virtual wrapper unaryMinus target: \
                 expression={unary:?} operand={operand:?} operand_ty={:?} target={:?}",
            info.ty(operand),
            info.resolved_operator_call(unary, "unaryMinus")
        );
    }
}

#[test]
fn companion_property_callable_reference_uses_the_shared_companion_record() {
    let Some(stdlib) = crate::toolchain::stdlib_jar() else {
        return;
    };
    let source = "import kotlin.reflect.KProperty0\n\
             class Sample { companion object { val maxValue = 1 } }\n\
             fun box(): String {\n\
                 val property: KProperty0<Int> = Sample::maxValue\n\
                 return if (property.get() == 1) \"OK\" else \"fail\"\n\
             }";
    let mut diagnostics = DiagSink::new();
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![stdlib]));
    let platform = initialized_jvm_libraries(classpath);
    let (_, symbols, info) =
        crate::frontend::analyze_source(source, jvm_platform(Box::new(platform)), &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");
    {
        let module = crate::module_symbols::ModuleSymbols::new(&symbols);
        let outer = module
            .classifier(crate::types::type_name("Sample"))
            .expect("outer classifier");
        let companion = outer
            .companion_object
            .as_ref()
            .map(|(_, companion)| *companion)
            .expect("companion object");
        let companion_shape = module.classifier(companion).expect("companion classifier");
        assert!(companion_shape.declared_callables.contains_key("maxValue"));
        let resolver = crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
            &*symbols.libraries,
            &module,
            &[],
        );
        assert!(resolver
            .select_symbol(
                crate::symbol_resolver::SymRecv::Value(Ty::obj_name(companion)),
                "maxValue",
                &[],
                &[],
            )
            .and_then(crate::symbol_resolver::Symbol::property_ref)
            .is_some());
    }
    assert_no_diags(&diagnostics);
}

#[test]
fn source_companion_classifier_is_available_during_signature_collection() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "class MyClass { companion object }\n\
             fun MyClass.Companion.ok() = \"OK\"\n\
             val ref = MyClass.Companion::ok",
        &mut diagnostics,
    );
    let files = vec![file];
    let _symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );

    assert_no_diags(&diagnostics);
}

#[test]
fn nested_property_infers_from_a_generic_companion_call() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        "class C {\n\
                 companion object { private fun <T> create() = C() }\n\
                 class ZZZ { val c = C.create<String>() }\n\
             }",
        &mut diagnostics,
    );
    let declaration_names = file
        .decls
        .iter()
        .filter_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class) => Some(class.name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(declaration_names, ["C", "C.Companion", "C.ZZZ"]);

    let files = vec![file];
    let symbols = collect_signatures_with_cp(
        &files,
        Box::new(crate::libraries::EmptySymbolSource),
        &mut diagnostics,
    );
    assert_eq!(
        symbols
            .class_by_type_name(type_name("C$Companion"))
            .and_then(|class| class.method("create"))
            .map(|method| method.ret),
        Some(Ty::obj("C"))
    );
    assert_eq!(
        symbols
            .class_by_type_name(type_name("C$ZZZ"))
            .and_then(|class| class.declared_props.get("c"))
            .map(|property| property.ty),
        Some(Ty::obj("C"))
    );
    assert_no_diags(&diagnostics);
}
