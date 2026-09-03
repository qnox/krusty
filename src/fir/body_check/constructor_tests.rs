use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;
use crate::ast::{Decl, FunBody};
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

fn module_target(call: &FirConstructorCall) -> crate::fir::CallableId {
    match &call.target {
        FirConstructorTarget::Module(target) => *target,
        FirConstructorTarget::External { .. } => panic!("expected a module constructor"),
    }
}

#[test]
fn primary_constructor_call_keeps_stable_constructor_identity_and_argument_slot() {
    let (body, index) = checked_function_body(
        "class Box(val value: Int)\nfun make(): Box = Box(42)\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("constructor call")
        .kind
    else {
        panic!("source construction must become checked constructor-call FIR")
    };
    assert!(index.callable(module_target(call)).is_some());
    assert!(call.outer_receiver.is_none());
    assert_eq!(call.arguments.len(), 1);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
}

#[test]
fn reference_array_factory_rebinds_to_the_selected_constructor_parameter() {
    let (body, _) = checked_function_body_with_platform(
        "import kotlin.reflect.KClass\n\
         class Left\n\
         class Right\n\
         class Holder(val classes: Array<KClass<*>>)\n\
         fun make(): Holder = Holder(arrayOf(Left::class, Right::class))\n",
        "make",
        super::test_support::jvm_semantics(),
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("source constructor")
        .kind
    else {
        panic!("contextual array factory must remain inside a checked constructor call")
    };
    assert_eq!(call.arguments.len(), 1);
}

#[test]
fn source_constructor_accepts_suspend_callable_value_through_sam_parameter() {
    let (body, _) = checked_function_body(
        "fun interface Foo<P> : suspend (P) -> Unit\n\
         fun interface Foo2<P> : suspend (P) -> Unit\n\
         class Bar<P>(foo: Foo<P>)\n\
         fun <P> create(foo: Foo2<P>): Bar<P> = Bar(foo)\n",
        "create",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("source constructor")
        .kind
    else {
        panic!("Bar(foo) must remain a checked source constructor call")
    };
    let [FirCallArgument::Expression {
        conversion:
            Some(FirConversion {
                kind: FirConversionKind::Sam(sam),
                ..
            }),
        ..
    }] = call.arguments.as_ref()
    else {
        panic!("the selected Foo parameter must retain its checked SAM conversion")
    };
    let sam = body.sam_conversion(*sam).expect("body-local SAM target");
    assert_eq!(sam.classifier, crate::types::type_name("Foo"));
    assert!(sam.suspend);
    assert_eq!(sam.parameters.len(), 1);
    assert_eq!(sam.result.get(), Ty::Unit);
}

#[test]
fn source_classifier_shadows_same_named_builtin_array_constructor() {
    let (body, index) = checked_function_body(
        "value class UIntArray(private val storage: IntArray) {\n\
             val size get() = storage.size\n\
         }\n\
         fun make(): UIntArray = UIntArray(intArrayOf(1, 2, 3))\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("source classifier construction")
        .kind
    else {
        panic!("source classifier must win over the same-named builtin array constructor")
    };
    let declaration = index
        .callable(module_target(call))
        .expect("stable source constructor")
        .declaration;
    let owner = index
        .declaration_anchor(declaration)
        .and_then(|anchor| anchor.owner)
        .expect("constructor classifier owner");
    assert!(index
        .classifier_header(owner)
        .is_some_and(|classifier| classifier.classifier == crate::types::type_name("UIntArray")));
}

#[test]
fn secondary_constructor_call_keeps_selected_constructor_identity() {
    let (primary_body, primary_index) = checked_function_body(
        "class Box(val value: Int) { constructor(text: String): this(text.length) }\n\
         fun make(): Box = Box(42)\n",
        "make",
    );
    let FirExprKind::ConstructorCall(primary) = &primary_body
        .expr(root_expression(&primary_body))
        .expect("primary constructor call")
        .kind
    else {
        panic!("primary construction must become checked constructor-call FIR")
    };
    let (secondary_body, secondary_index) = checked_function_body(
        "class Box(val value: Int) { constructor(text: String): this(text.length) }\n\
         fun make(): Box = Box(\"answer\")\n",
        "make",
    );
    let FirExprKind::ConstructorCall(secondary) = &secondary_body
        .expr(root_expression(&secondary_body))
        .expect("secondary constructor call")
        .kind
    else {
        panic!("secondary construction must become checked constructor-call FIR")
    };
    assert!(primary_index.callable(module_target(primary)).is_some());
    assert!(secondary_index.callable(module_target(secondary)).is_some());
    assert_ne!(
        primary_index
            .callable(module_target(primary))
            .unwrap()
            .declaration,
        secondary_index
            .callable(module_target(secondary))
            .unwrap()
            .declaration,
    );
}

#[test]
fn generic_secondary_constructor_keeps_inferred_class_substitution() {
    let (body, index) = checked_function_body(
        "class Box<T>(val first: T, val second: T) { constructor(value: T): this(value, value) }\n\
         fun make(): Any = Box(\"answer\")\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("generic secondary constructor call")
        .kind
    else {
        panic!("generic secondary construction must become checked constructor-call FIR")
    };
    assert!(index.callable(module_target(call)).is_some());
    assert_eq!(call.arguments.len(), 1);
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Expression { parameter: 0, .. }
    ));
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::String);
}

#[test]
fn generic_vararg_secondary_constructor_keeps_inferred_class_substitution() {
    let (body, index) = checked_function_body(
        "class Box<T>(val value: T) { constructor(vararg values: T): this(values[0]) }\n\
         fun make(): Any = Box(\"first\", \"second\")\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("generic vararg secondary constructor call")
        .kind
    else {
        panic!("generic vararg secondary construction must become checked constructor-call FIR")
    };
    assert!(index.callable(module_target(call)).is_some());
    assert_eq!(call.arguments.len(), 2);
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::String);
}

#[test]
fn generic_vararg_secondary_constructor_infers_spread_and_named_array_substitutions() {
    let source = "class Box<T>(val value: T) { constructor(vararg values: T): this(values[0]) }\n\
         fun spread(values: Array<String>): Any = Box(*values)\n\
         fun named(values: Array<String>): Any = Box(values = values)\n";

    for function in ["spread", "named"] {
        let (body, _) = checked_function_body(source, function);
        let FirExprKind::ConstructorCall(call) = &body
            .expr(root_expression(&body))
            .expect("generic vararg secondary constructor call")
            .kind
        else {
            panic!("{function} must become a checked constructor call")
        };
        assert_eq!(call.substitutions.len(), 1, "{function}");
        assert_eq!(call.substitutions[0].value.get(), Ty::String, "{function}");
    }
}

#[test]
fn constructor_default_is_an_explicit_checked_argument_decision() {
    let (body, _) = checked_function_body(
        "class Box(val value: Int = 42)\nfun make(): Box = Box()\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("constructor call")
        .kind
    else {
        panic!("defaulted construction must become checked constructor-call FIR")
    };
    assert!(matches!(
        call.arguments[0],
        FirCallArgument::Default { parameter: 0, .. }
    ));
}

#[test]
fn generic_constructor_keeps_class_type_parameter_substitution() {
    let (body, index) = checked_function_body(
        "class Box<T>(val value: T)\nfun make(): Box<String> = Box(\"answer\")\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("generic constructor call")
        .kind
    else {
        panic!("generic construction must become checked constructor-call FIR")
    };
    assert_eq!(call.substitutions.len(), 1);
    let constructor = index
        .callable(module_target(call))
        .expect("constructor identity")
        .declaration;
    let classifier = index
        .declaration_anchor(constructor)
        .expect("constructor anchor")
        .owner
        .expect("classifier owner");
    assert_eq!(
        Some(call.substitutions[0].parameter),
        index.type_parameter(classifier, 0).map(Into::into),
    );
}

#[test]
fn generic_constructor_infers_its_argument_from_an_expected_supertype() {
    let (body, _) = checked_function_body(
        "interface Step<I>\n\
         interface Terminal<I> : Step<I>\n\
         class Owner\n\
         class Success<I>(val result: Int) : Terminal<I>\n\
         class Keep : Step<Owner>\n\
         fun next(done: Boolean): Step<Owner> = if (done) Success(1) else Keep()\n",
        "next",
    );

    let success = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::ConstructorCall(call) = &expression.kind else {
            return None;
        };
        expression
            .ty
            .get()
            .obj_internal()
            .is_some_and(|owner| owner.matches("Success"))
            .then_some((expression, call))
    });
    let Some((expression, call)) = success else {
        panic!("expected-supertype construction must remain checked FIR")
    };
    assert_eq!(
        expression.ty.get(),
        Ty::obj_args("Success", &[Ty::obj("Owner")])
    );
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::obj("Owner"));
}

#[test]
fn expected_supertype_contextualizes_nested_constructor_lambdas() {
    let (body, _) = checked_function_body_with_platform(
        "class Yield<T>(val factory: () -> (() -> T?)) : Iterable<T> {\n\
             override fun iterator(): Iterator<T> = null as Iterator<T>\n\
         }\n\
         fun <T> Iterable<T>.make(): Iterable<T> = Yield {\n\
             val iterator = this.iterator()\n\
             { if (iterator.hasNext()) iterator.next() else null }\n\
         }\n",
        "make",
        jvm_stdlib_semantics(),
    );

    let root = body.expr(root_expression(&body)).expect("constructor call");
    let FirExprKind::ConstructorCall(call) = &root.kind else {
        panic!("nested-lambda construction must become checked constructor FIR")
    };
    assert_eq!(call.substitutions.len(), 1);
    assert!(matches!(call.substitutions[0].value.get(), Ty::TyParam(..)));
    assert!(matches!(root.ty.get().type_args(), [Ty::TyParam(..)]));
}

#[test]
fn expected_supertype_completes_nested_constructor_variable_hidden_by_star_projection() {
    let (body, _) = checked_function_body(
        "sealed class Result<I, V> {\n\
             class Error<I, V>(val child: Error<I, *>?, val rest: I) : Result<I, V>()\n\
         }\n\
         fun <I, V> copy(error: Result.Error<I, *>, rest: I): Result<I, V> =\n\
             Result.Error(error, rest)\n",
        "copy",
    );

    let root = body.expr(root_expression(&body)).expect("constructor call");
    let FirExprKind::ConstructorCall(call) = &root.kind else {
        panic!("nested generic construction must become checked constructor FIR")
    };
    assert_eq!(call.substitutions.len(), 2);
    assert!(call
        .substitutions
        .iter()
        .all(|substitution| matches!(substitution.value.get(), Ty::TyParam(..))));
}

#[test]
fn selected_constructor_lambda_propagates_its_fixed_result_into_when_branches() {
    let (body, _) = checked_function_body(
        r#"
class Parser<I, V>(val parse: (I) -> Result<I, V>) {
    operator fun invoke(input: I): Result<I, V> = parse(input)

    fun <M, R> mapJoin(
        select: (V) -> Parser<I, M>,
        project: (V, M) -> R,
    ): Parser<I, R> = Parser { input ->
        when (val first = this(input)) {
            is Result.Error -> Result.Error(first.message, first.child, first.rest)
            is Result.Value -> when (val second = select(first.value)(first.rest)) {
                is Result.Error -> Result.Error(second.message, second.child, second.rest)
                is Result.Value -> Result.Value(project(first.value, second.value), second.rest)
            }
        }
    }
}

sealed class Result<I, V> {
    class Value<I, V>(val value: V, val rest: I) : Result<I, V>()
    class Error<I, V>(val message: String, val child: Error<I, *>?, val rest: I) : Result<I, V>()
}
"#,
        "mapJoin",
    );

    // Building the checked body also checks and dispatches the nested lambda body. Before the
    // selected return expectation was propagated, either Error construction failed there with a
    // raw classifier type and the helper rejected the source before returning this root body.
    let root = body
        .expr(root_expression(&body))
        .expect("Parser construction");
    let FirExprKind::ConstructorCall(call) = &root.kind else {
        panic!("mapJoin result must remain a checked constructor call")
    };
    assert_eq!(root.ty.get().type_args().len(), 2);
    assert_eq!(call.substitutions.len(), 2);
    assert!(call
        .substitutions
        .iter()
        .all(|substitution| matches!(substitution.value.get(), Ty::TyParam(..))));
}

#[test]
fn expected_primary_constructor_result_fixes_a_member_generic_lambda_parameter() {
    let (body, _) = checked_function_body(
        "class Parser<I, V>(val parse: (I) -> V) {\n\
             fun <R> map(project: (V) -> R): Parser<I, R> {\n\
                 return Parser({ input -> project(parse(input)) })\n\
             }\n\
         }\n",
        "map",
    );

    let constructor = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::ConstructorCall(call) => Some((expression, call)),
            _ => None,
        })
        .expect("member-generic constructor call");
    assert!(matches!(
        constructor.0.ty.get().type_args(),
        [Ty::TyParam(..), Ty::TyParam(..)]
    ));
    assert_eq!(constructor.1.substitutions.len(), 2);
    assert!(constructor
        .1
        .substitutions
        .iter()
        .all(|substitution| matches!(substitution.value.get(), Ty::TyParam(..))));
}

#[test]
fn constructor_underscore_type_argument_is_replaced_by_the_inferred_substitution() {
    let (body, _) = checked_function_body(
        "class Box<E : Double, A : Any>(val value: A)\n\
         fun make(): Any = Box<Double, _>(\"answer\")\n",
        "make",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("generic constructor call");
    let FirExprKind::ConstructorCall(call) = &root.kind else {
        panic!("generic construction must become checked constructor-call FIR")
    };
    assert_eq!(root.ty.get().type_args(), &[Ty::Double, Ty::String]);
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(call.substitutions[0].value.get(), Ty::Double);
    assert_eq!(call.substitutions[1].value.get(), Ty::String);
}

#[test]
fn constructor_pcla_commits_nested_lambda_constraints_before_fir() {
    let (body, _) = checked_function_body(
        "class Builder<T : Any>(val block: (Builder<T>.() -> Unit)? = null) {\n\
             var consume: ((T) -> Unit)? = null\n\
         }\n\
         fun consumeInt(value: Int): Unit {}\n\
         fun make(): Any {\n\
             val value = Builder { consume = { consumeInt(it) } }\n\
             return value\n\
         }\n",
        "make",
    );

    let constructor = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::ConstructorCall(call) = &expression.kind else {
            return None;
        };
        Some((expression, call))
    });
    let Some((expression, call)) = constructor else {
        panic!("PCLA constructor must remain a checked constructor call")
    };
    assert_eq!(expression.ty.get(), Ty::obj_args("Builder", &[Ty::Int]));
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::Int);
    for raw in 0..body.expression_count() {
        let expression = body
            .expr(FirExprId::from_raw(raw as u32))
            .expect("dense FIR expression arena");
        assert!(
            !expression.ty.get().mentions_ty_param(),
            "non-generic caller FIR retained an open constructor parameter: {:?}",
            expression.kind,
        );
    }
}

#[test]
fn cross_file_generic_primary_constructor_binds_the_stable_declaration() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[
            SourceInput::kotlin("class Box<T>(val value: T)\n").with_file_stem("Box"),
            SourceInput::kotlin("fun make(): Box<String> = Box(\"answer\")\n")
                .with_file_stem("Make"),
        ],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let root = analysis.files[1]
        .decls
        .iter()
        .find_map(|declaration| match analysis.files[1].decl(*declaration) {
            Decl::Fun(function) if function.name == "make" => match function.body {
                FunBody::Expr(root) => Some(root),
                FunBody::Block(_) | FunBody::None => None,
            },
            Decl::Fun(_) | Decl::Property(_) | Decl::Class(_) => None,
        })
        .expect("make expression body");
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        &analysis.files[1],
        analysis.types[1].as_ref().expect("checked caller"),
        SourceFileId::from_raw(1),
        BodyOwnerId::from_raw(0),
        root,
        streamed.module.index(),
        &mut origins,
    )
    .expect("cross-file generic construction must become checked FIR");
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("generic constructor call")
        .kind
    else {
        panic!("generic construction must become checked constructor-call FIR")
    };
    assert!(streamed
        .module
        .index()
        .callable(module_target(call))
        .is_some());
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::String);
}

#[test]
fn projected_expected_constructor_result_does_not_erase_argument_inference() {
    let (body, _) = checked_function_body(
        "interface OperandType<J>\n\
         object SInt32 : OperandType<Int>\n\
         class LoadConstant<J, T : OperandType<J>>(val value: J, val type: T)\n\
         fun consume(value: LoadConstant<*, *>) {}\n\
         fun test() { consume(LoadConstant(0, SInt32)) }\n",
        "test",
    );
    let constructor = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::ConstructorCall(call) if call.substitutions.len() == 2 => Some(call),
            _ => None,
        })
        .expect("generic constructor nested in the projected argument");
    assert_eq!(constructor.substitutions[0].value.get(), Ty::Int);
    assert_eq!(constructor.substitutions[1].value.get(), Ty::obj("SInt32"));
}

#[test]
fn enclosing_constructor_bound_contextualizes_a_postponed_builder_result() {
    let (body, index) = checked_function_body(
        "class Target\n\
         class Buildee<T> { fun set(value: T) {} }\n\
         fun <T> build(instructions: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()\n\
         class Holder<T : Any>(val buildee: Buildee<T>)\n\
         fun consume(value: Buildee<Any>) {}\n\
         fun test() {\n\
             val holder = Holder(build { set(Target()) })\n\
             consume(holder.buildee)\n\
         }\n",
        "test",
    );
    let holder = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::ConstructorCall(call) if call.substitutions.len() == 1 => {
                Some((expression, call))
            }
            _ => None,
        })
        .expect("enclosing generic constructor");
    assert_eq!(holder.1.substitutions[0].value.get(), Ty::obj("kotlin/Any"));

    let (build_expression, build) = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::Call(call) = &expression.kind else {
                return None;
            };
            let target = call.target.module()?;
            (index.callable_name(target) == Some("build")).then_some((expression, call))
        })
        .expect("postponed builder call");
    let expected = Ty::obj_args("Buildee", &[Ty::obj("kotlin/Any")]);
    assert_eq!(build_expression.ty.get(), expected);
    let [substitution] = build.substitutions.as_ref() else {
        panic!("the builder must publish its enclosing-context solution")
    };
    assert_eq!(substitution.value.get(), Ty::obj("kotlin/Any"));
    assert!(!substitution.value.get().mentions_pending());
}

#[test]
fn reparsed_cross_file_constructor_retains_nullable_top_type_argument() {
    let inputs = [
        SourceInput::kotlin("class Box<T>(value: T) { fun accept(other: T) {} }\n")
            .with_file_stem("Box"),
        SourceInput::kotlin("fun use(value: Any?) { Box(value).accept(value) }\n")
            .with_file_stem("Use"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
}

#[test]
fn inner_constructor_keeps_own_and_captured_type_parameter_substitutions() {
    let (body, index) = checked_function_body(
        "class Outer<OP> {\n\
             inner class Inner<IP>\n\
             fun <T> make(): Inner<T> = Inner<T>()\n\
         }\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("inner constructor call")
        .kind
    else {
        panic!("inner construction must become checked constructor-call FIR")
    };
    let constructor = index
        .callable(module_target(call))
        .expect("constructor identity")
        .declaration;
    let inner = index
        .declaration_anchor(constructor)
        .expect("constructor anchor")
        .owner
        .expect("inner classifier");
    let outer = index
        .declaration_header(inner)
        .expect("inner classifier header")
        .owner
        .expect("outer classifier");
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(
        Some(call.substitutions[0].parameter),
        index.type_parameter(inner, 0).map(Into::into),
    );
    assert_eq!(
        Some(call.substitutions[1].parameter),
        index.type_parameter(outer, 0).map(Into::into),
    );
}

#[test]
fn inner_constructor_parameter_uses_its_captured_outer_type_parameter() {
    let (body, index) = checked_function_body(
        "interface Source<T>\n\
         class Outer<T> {\n\
             inner class Inner(value: Source<T>)\n\
             fun make(value: Source<T>): Inner = Inner(value)\n\
         }\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("inner constructor call")
        .kind
    else {
        panic!("captured-parameter construction must become checked FIR")
    };
    assert_eq!(call.substitutions.len(), 1);
    let constructor = index
        .callable(module_target(call))
        .expect("constructor identity")
        .declaration;
    let inner = index
        .declaration_anchor(constructor)
        .expect("constructor anchor")
        .owner
        .expect("inner classifier");
    let outer = index
        .declaration_header(inner)
        .expect("inner header")
        .owner
        .expect("outer classifier");
    assert_eq!(
        Some(call.substitutions[0].parameter),
        index.type_parameter(outer, 0).map(Into::into),
    );
    assert!(matches!(call.substitutions[0].value.get(), Ty::TyParam(..)));
}

#[test]
fn inherited_inner_constructor_captures_arguments_from_applied_supertype() {
    let (body, _) = checked_function_body(
        "open class C<T> {\n\
             inner class A<U>(val x: T?, val y: U)\n\
             class D : C<Nothing>() {\n\
                 fun make() = A<String>(null, \"OK\")\n\
             }\n\
         }\n",
        "make",
    );
    let expression = body
        .expr(root_expression(&body))
        .expect("inherited inner constructor");
    assert_eq!(expression.ty.get().type_args(), &[Ty::String, Ty::Nothing]);
    let FirExprKind::ConstructorCall(call) = &expression.kind else {
        panic!("inherited inner construction must become checked FIR")
    };
    assert!(call.outer_receiver.is_some());
    assert_eq!(call.substitutions.len(), 2);
    assert_eq!(call.substitutions[0].value.get(), Ty::String);
    assert_eq!(call.substitutions[1].value.get(), Ty::Nothing);
}

#[test]
fn callable_property_contextually_types_an_empty_inner_constructor() {
    let _ = checked_function_body(
        "open class A<X : String>(val x: X) {\n\
             inner class B<Y> { fun value(): X = x }\n\
             val reference = B<Int>::value\n\
             fun use(): String = reference(B())\n\
         }\n",
        "use",
    );
}

#[test]
fn callable_property_contextualizes_inner_own_parameter_after_binding_outer() {
    let (body, _) = checked_function_body(
        "class Outer<A> {\n\
             inner class Inner<B>\n\
             val accept: (Inner<Int>) -> String = { \"OK\" }\n\
         }\n\
         fun use(): String = Outer<Int>().accept(Outer<Int>().Inner())\n",
        "use",
    );

    let inner = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| {
            expression
                .ty
                .get()
                .obj_internal()
                .is_some_and(|owner| owner.matches("Outer$Inner"))
        })
        .expect("contextual inner construction");
    assert_eq!(inner.ty.get().type_args(), &[Ty::Int, Ty::Int]);
    let FirExprKind::ConstructorCall(call) = &inner.kind else {
        panic!("contextual inner value must remain a checked constructor call")
    };
    assert_eq!(call.substitutions.len(), 2);
    assert!(call
        .substitutions
        .iter()
        .all(|substitution| substitution.value.get() == Ty::Int));
}

#[test]
fn safe_inner_constructor_keeps_guarded_outer_receiver_and_stable_target() {
    let (body, index) = checked_function_body(
        "class Outer { inner class Inner }\n\
         fun make(outer: Outer?): Outer.Inner? = outer?.Inner()\n",
        "make",
    );
    let FirExprKind::SafeCall { receiver, selector } = &body
        .expr(root_expression(&body))
        .expect("safe constructor call")
        .kind
    else {
        panic!("safe inner construction must retain its null guard")
    };
    let FirExprKind::ConstructorCall(call) = &body
        .expr(*selector)
        .expect("inner constructor selector")
        .kind
    else {
        panic!("safe selector must be a checked constructor call")
    };
    assert_eq!(call.outer_receiver, Some(*receiver));
    assert!(index.callable(module_target(call)).is_some());
    assert!(call.substitutions.is_empty());
}

#[test]
fn receiver_lambda_opens_inner_classifier_scope_and_binds_its_outer_value() {
    let (body, index) = checked_function_body_with_platform(
        "class Outer { inner class Inner }\n\
         fun make(): Outer.Inner = with(Outer()) { Inner() }\n",
        "make",
        super::test_support::jvm_semantics(),
    );
    let lambda = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            match &expression.kind {
                FirExprKind::Lambda { body, .. } => Some(body.as_ref()),
                _ => None,
            }
        })
        .expect("with must retain its checked receiver-lambda body");
    let call = (0..lambda.expression_count())
        .find_map(|raw| {
            let expression = lambda.expr(FirExprId::from_raw(raw as u32))?;
            match &expression.kind {
                FirExprKind::ConstructorCall(call) if call.outer_receiver.is_some() => Some(call),
                _ => None,
            }
        })
        .expect("inner construction must retain its receiver-lambda outer value");
    assert!(index.callable(module_target(call)).is_some());
    let outer = call
        .outer_receiver
        .expect("bound inner constructor receiver");
    assert!(matches!(
        lambda.expr(outer.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn labeled_super_inner_constructor_keeps_the_selected_outer_receiver_coordinate() {
    let source = "open class Base { inner class Item(val text: String) }\n\
                  class Host : Base() {\n\
                      inner class Nested : Base() {\n\
                          fun immediate() = super@Nested.Item(\"nested\")\n\
                          fun enclosing() = super@Host.Item(\"host\")\n\
                      }\n\
                  }\n";
    for function in ["immediate", "enclosing"] {
        let (body, index) = checked_function_body(source, function);
        let FirExprKind::ConstructorCall(call) = &body
            .expr(root_expression(&body))
            .expect("inner constructor call")
            .kind
        else {
            panic!("labeled super inner construction must become a checked constructor call")
        };
        assert!(index.callable(module_target(call)).is_some());
        let receiver = call
            .outer_receiver
            .expect("inner constructor must retain its selected outer receiver");
        let receiver_kind = body.expr(receiver.value).map(|expression| &expression.kind);
        match function {
            "immediate" => assert!(
                matches!(
                    receiver_kind,
                    Some(FirExprKind::ImplicitReceiver {
                        current: true,
                        depth: 0,
                    })
                ),
                "{function} selected {receiver_kind:?}"
            ),
            "enclosing" => {
                let Some(FirExprKind::EnclosingReceiver { path }) = receiver_kind else {
                    panic!("{function} selected {receiver_kind:?}")
                };
                let classifiers = path
                    .iter()
                    .map(|declaration| {
                        index
                            .classifier_header(*declaration)
                            .expect("selected enclosing classifier edge")
                            .classifier
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    classifiers,
                    [crate::types::type_name("Host$Nested")],
                    "the FIR coordinate must identify the exact enclosing-instance edge",
                );
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn constructor_expected_result_preserves_nullable_caller_type_parameter() {
    let (body, _) = checked_function_body(
        "data class Pair<A, B>(val first: A, val second: B)\n\
         fun <T, R> pair(first: T, second: R): Pair<T, R?> = Pair(first, second)\n",
        "pair",
    );
    let root = body.expr(root_expression(&body)).expect("constructor call");
    assert!(matches!(root.kind, FirExprKind::ConstructorCall(_)));
    assert_eq!(Some(root.ty), body.result_type());
}

#[test]
fn enclosing_generic_call_contextualizes_a_zero_evidence_constructor_result() {
    let (body, _) = checked_function_body(
        "class TypeToken<T>\n\
         interface Context<C : Any> {\n\
             companion object {\n\
                 operator fun <C : Any> invoke(type: TypeToken<C>, value: C): Context<C> =\n\
                     null as Context<C>\n\
             }\n\
         }\n\
         fun <C : Any> make(value: C): Context<C> = Context(TypeToken(), value)\n",
        "make",
    );

    let result = body.result_type().expect("generic caller result").get();
    let [context_argument] = result.type_args() else {
        panic!("Context<C> must retain the caller's symbolic argument")
    };
    let token = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| {
            matches!(expression.kind, FirExprKind::ConstructorCall(_))
                && expression.ty.get().obj_internal() == Some(crate::types::type_name("TypeToken"))
        })
        .expect("nested TypeToken construction");
    assert_eq!(
        token.ty.get(),
        Ty::obj_args("TypeToken", &[*context_argument]),
        "the selected outer parameter must complete the constructor's type argument before FIR",
    );
}

#[test]
fn classifier_value_invoke_honors_explicit_type_arguments_during_overload_selection() {
    let (body, _) = checked_function_body(
        "class TypeToken<T>\n\
         interface Context<C : Any> {\n\
             companion object {\n\
                 operator fun <C : Any> invoke(type: TypeToken<C>, value: C): Context<C> =\n\
                     null as Context<C>\n\
                 operator fun <C : Any> invoke(type: TypeToken<C>, provider: () -> C): Context<C> =\n\
                     null as Context<C>\n\
             }\n\
         }\n\
         fun <C : Any> make(provider: () -> C): Context<C> =\n\
             Context<C>(TypeToken()) { provider() }\n",
        "make",
    );

    let root = body
        .expr(root_expression(&body))
        .expect("selected companion invoke");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("classifier-value invocation must remain a checked FIR call")
    };
    assert_eq!(Some(root.ty), body.result_type());
    assert_eq!(call.substitutions.len(), 1);
    assert!(matches!(call.substitutions[0].value.get(), Ty::TyParam(..)));
}

#[test]
fn invariant_expected_constructor_result_outweighs_a_bottom_null_argument() {
    let (body, _) = checked_function_body(
        "class Inv<T>(val value: T?)\n\
         fun <R> make(value: R?): Inv<R> {\n\
             if (value != null) return null as Inv<R>\n\
             return Inv(value)\n\
         }\n",
        "make",
    );

    let construction = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::ConstructorCall(_)))
        .expect("contextual generic construction");
    assert_eq!(
        Some(construction.ty),
        body.result_type(),
        "the declared invariant result equality must retain the caller's symbolic R",
    );
}

#[test]
fn invariant_expected_result_contextualizes_default_only_constructor() {
    let (body, _) = checked_function_body(
        "class A<T>(val value: T? = null)\n\
         fun make(): A<Any?> = A()\n",
        "make",
    );

    let construction = body
        .expr(root_expression(&body))
        .expect("default-only generic construction");
    assert!(matches!(construction.kind, FirExprKind::ConstructorCall(_)));
    assert_eq!(
        Some(construction.ty),
        body.result_type(),
        "the invariant expected result must retain nullable Any",
    );
}

#[test]
fn sibling_builder_constraint_contextualizes_an_empty_generic_constructor() {
    let _ = checked_function_body(
        "class Target\n\
         class Box<T>\n\
         class Builder<T> { fun set(value: T) {} }\n\
         fun <T> build(block: Builder<T>.() -> Unit): Builder<T> = Builder<T>()\n\
         fun test() {\n\
             build {\n\
                 if (true) set(Box()) else set(Box<Target>())\n\
             }\n\
         }\n",
        "test",
    );
}

#[test]
fn projected_generic_constructor_parameter_keeps_symbolic_argument_in_fir() {
    let (body, _) = checked_function_body(
        "class Buildee<T>\n\
         class InBuildee<in T>(val buildee: Buildee<in T>)\n\
         fun <T> make(buildee: Buildee<T>): InBuildee<T> = InBuildee(buildee)\n",
        "make",
    );

    let root = body.expr(root_expression(&body)).expect("constructor call");
    assert!(matches!(root.kind, FirExprKind::ConstructorCall(_)));
    assert!(matches!(root.ty.get().type_args(), [Ty::TyParam(..)]));
}

#[test]
fn nested_classifier_is_visible_by_simple_name_in_its_own_supertype_header() {
    let (body, _) = checked_function_body(
        "interface SomeInterface<T>\n\
         object Container {\n\
             private inline fun <reified T> someMethod() = object : SomeInterface<T> {}\n\
             class SomeClass : SomeInterface<SomeClass> by someMethod()\n\
         }\n\
         fun make(): Container.SomeClass = Container.SomeClass()\n",
        "make",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("nested construction");
    assert!(matches!(root.kind, FirExprKind::ConstructorCall(_)));
    assert_eq!(root.ty.get(), Ty::obj("Container$SomeClass"));
}

#[test]
fn projected_constructor_arguments_infer_without_result_expectation() {
    let source = r#"
class Buildee<T>
class InBuildee<in T>(val buildee: Buildee<in T>)
class OutBuildee<out T>(val buildee: Buildee<out T>)

fun <T> makeIn(buildee: Buildee<T>) = InBuildee(buildee)
fun <T> makeOut(buildee: Buildee<T>) = OutBuildee(buildee)
"#;

    for function in ["makeIn", "makeOut"] {
        let (body, _) = checked_function_body(source, function);
        let root = body.expr(root_expression(&body)).expect("constructor call");
        assert!(matches!(root.kind, FirExprKind::ConstructorCall(_)));
        assert!(
            matches!(root.ty.get().type_args(), [Ty::TyParam(..)]),
            "{function} inferred {:?}",
            root.ty.get()
        );
    }
}

#[test]
fn constructor_vararg_elements_keep_source_order_and_spread_decisions() {
    let (body, _) = checked_function_body(
        "class Box(vararg values: String)\n\
         fun make(values: Array<String>): Box = Box(\"first\", *values, \"last\")\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("vararg constructor call")
        .kind
    else {
        panic!("vararg construction must become checked constructor-call FIR")
    };
    assert_eq!(call.arguments.len(), 3);
    for (index, spread) in [false, true, false].into_iter().enumerate() {
        let FirCallArgument::Vararg {
            parameter: 0,
            elements,
            ..
        } = &call.arguments[index]
        else {
            panic!("each source vararg operand must retain its own ordered FIR entry")
        };
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].spread, spread);
    }
}

#[test]
fn omitted_constructor_vararg_is_an_explicit_empty_pack() {
    let (body, _) = checked_function_body(
        "class Box(vararg values: String)\nfun make(): Box = Box()\n",
        "make",
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("empty vararg constructor call")
        .kind
    else {
        panic!("empty vararg construction must become checked constructor-call FIR")
    };
    assert!(matches!(
        &call.arguments[0],
        FirCallArgument::Vararg {
            parameter: 0,
            elements,
            ..
        } if elements.is_empty()
    ));
}

#[test]
fn dependency_constructor_keeps_backend_neutral_external_identity() {
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
            crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
        )),
    ));
    let (body, _) = checked_function_body_with_platform(
        "fun make(): kotlin.Pair<Int, String> = kotlin.Pair(1, \"answer\")\n",
        "make",
        platform,
    );
    let FirExprKind::ConstructorCall(call) = &body
        .expr(root_expression(&body))
        .expect("constructor call")
        .kind
    else {
        panic!("dependency construction must become checked constructor-call FIR")
    };
    let FirConstructorTarget::External {
        classifier,
        parameters,
        ..
    } = &call.target
    else {
        panic!("dependency constructor must not fabricate a module declaration")
    };
    assert!(classifier.matches("kotlin/Pair"));
    assert_eq!(parameters.len(), 2);
    assert_eq!(call.arguments.len(), 2);
}
