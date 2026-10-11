//! A lambda whose function type takes or returns a value class over a non-null reference cannot be
//! built by `LambdaMetafactory`: its `invoke` takes the carrier where `FunctionN.invoke` takes the
//! box. kotlinc compiles such a lambda to a class of its own, `final` over `Object` and implementing
//! the function type, with the lambda's body as its specialized `invoke`, an erased bridge that
//! unboxes and boxes the value class, its captures as final synthetic fields, and a shared
//! `INSTANCE` when it captures nothing. A value class over a primitive keeps the indy lambda.

use super::common;

const SRC: &str = "@JvmInline value class Tag(val name: String)\n\
    @JvmInline value class Count(val n: Int)\n\
    fun label(f: (Tag) -> String, t: Tag): String = f(t)\n\
    fun echo(f: (Tag?) -> Tag?, t: Tag?): Tag? = f(t)\n\
    fun total(f: (Count) -> Int, c: Count): Int = f(c)\n\
    class Owner(private val secret: String) {\n\
    \x20   fun read(t: Tag): String = label({ secret }, t)\n\
    }\n\
    fun box(): String {\n\
    \x20   val a = label({ it.name }, Tag(\"O\"))\n\
    \x20   val b = echo({ it }, Tag(\"K\"))\n\
    \x20   val none = echo({ it }, null)\n\
    \x20   var suffix = \"\"\n\
    \x20   val c = label({ suffix = \"!\"; it.name }, Tag(\"-\"))\n\
    \x20   val d = total({ it.n + 1 }, Count(1))\n\
    \x20   val e = Owner(\"s\").read(Tag(\"x\"))\n\
    \x20   val u = Tag(\"u\")\n\
    \x20   val f = label({ u.name }, Tag(\"v\"))\n\
    \x20   if (none != null || c != \"-\" || suffix != \"!\" || d != 2 || e != \"s\" || f != \"u\") return \"fail\"\n\
    \x20   return a + b!!.name\n\
    }\n";

/// kotlinc and krusty write `class` byte for byte alike.
fn assert_identical(class: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassLambdaClass",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    assert!(!built.reference_bytes.is_empty(), "kotlinc writes {class}");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} differs from kotlinc's:\n{}\n---\n{}",
        built.krusty,
        built.reference
    );
}

#[test]
fn a_captureless_lambda_over_a_reference_value_class_is_a_singleton_class() {
    assert_identical("ValueClassLambdaClassKt$box$a$1");
}

#[test]
fn a_nullable_value_class_lambda_boxes_null_safely_in_its_bridge() {
    assert_identical("ValueClassLambdaClassKt$box$b$1");
    assert_identical("ValueClassLambdaClassKt$box$none$1");
}

#[test]
fn a_capturing_lambda_class_stores_its_shared_cell_in_a_field() {
    assert_identical("ValueClassLambdaClassKt$box$c$1");
}

/// A captured value class is stored as its carrier, and the constructor taking it is a plain
/// package-private one: only a declared Kotlin parameter hides a constructor behind the
/// `DefaultConstructorMarker` accessor.
#[test]
fn a_captured_value_class_is_a_plain_constructor_parameter() {
    assert_identical("ValueClassLambdaClassKt$box$f$1");
}

#[test]
fn a_lambda_class_reads_its_owners_private_property_through_an_accessor() {
    assert_identical("Owner$read$1");
}

#[test]
fn lambda_classes_run() {
    common::expect_box_same_as_kotlinc(SRC, "ValueClassLambdaClass");
}

/// A constructor-reference adapter in a static delegated-property initializer is already shaped
/// for the callable-reference ABI. Discovering reified source closures in every emitted root must
/// not reclassify that adapter as an ordinary source lambda.
#[test]
fn a_value_class_constructor_reference_in_a_static_initializer_keeps_its_adapter_abi() {
    const SRC: &str = "class Marker\n\
        object Expected { val marker = Marker() }\n\
        interface Source {\n\
        companion object { val default: Token by lazy(::Token) }\n\
    }\n\
    @JvmInline value class Token(val marker: Marker = Expected.marker) : Source\n\
    fun box(): String = if (Source.default.marker === Expected.marker) \"OK\" else \"fail\"\n";
    common::expect_box_same_as_kotlinc(SRC, "StaticValueClassConstructorReference");
}

#[test]
fn a_source_value_class_lambda_in_a_static_initializer_is_realized_as_a_class() {
    const SRC: &str = "class Marker\n\
        object Expected { val marker = Marker() }\n\
        @JvmInline value class Token(val marker: Marker)\n\
        val factory: () -> Token = { Token(Expected.marker) }\n\
        fun box(): String = if (factory().marker === Expected.marker) \"OK\" else \"fail\"\n";
    let built = common::compare_with_kotlinc_plugin(
        "StaticValueClassLambda",
        SRC,
        "StaticValueClassLambdaKt$factory$1",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    assert!(
        !built.reference_bytes.is_empty(),
        "kotlinc writes the source lambda class"
    );
    assert_eq!(built.krusty_bytes, built.reference_bytes);
    common::expect_box_same_as_kotlinc(SRC, "StaticValueClassLambdaRun");
}

#[test]
fn a_generic_value_class_constructor_reference_in_a_static_initializer_keeps_its_adapter_abi() {
    const SRC: &str = "open class Marker\n\
        class ExactMarker : Marker()\n\
        object Expected { val marker = ExactMarker() }\n\
        interface Source {\n\
        companion object { val default: Token<ExactMarker> by lazy(::Token) }\n\
    }\n\
    @JvmInline value class Token<T : Marker>(val marker: T = Expected.marker as T) : Source\n\
    fun box(): String = if (Source.default.marker === Expected.marker) \"OK\" else \"fail\"\n";
    common::expect_box_same_as_kotlinc(SRC, "StaticGenericValueClassConstructorReference");
}

/// A lambda passed to a same-file inline function's `noinline` parameter is a value that function
/// receives, not a body it splices. The expansion materializes the value before lambda classes are
/// realized, so it is the class kotlinc writes.
#[test]
fn a_noinline_argument_of_an_inline_call_is_its_own_class() {
    const SRC: &str = "@JvmInline value class Tag(val name: String)\n\
        inline fun later(noinline f: (Tag) -> String, t: Tag): String = keep(f)(t)\n\
        fun keep(f: (Tag) -> String): (Tag) -> String = f\n\
        fun box(): String = later({ it.name }, Tag(\"OK\"))\n";
    let built = common::compare_with_kotlinc_plugin(
        "NoinlineLambdaClass",
        SRC,
        "NoinlineLambdaClassKt$box$1",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    assert!(
        !built.reference_bytes.is_empty(),
        "kotlinc writes NoinlineLambdaClassKt$box$1"
    );
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "NoinlineLambdaClassKt$box$1 differs from kotlinc's:\n{}\n---\n{}",
        built.krusty,
        built.reference
    );
    common::expect_box_same_as_kotlinc(SRC, "NoinlineLambdaClass");
}

/// A lambda the metafactory cannot adapt whose body declares a local function would need that
/// function moved into its class, which is not realized yet. The backend reports it rather than
/// emitting an `invokedynamic` that cannot link.
#[test]
fn a_lambda_class_nesting_a_local_function_is_reported_not_left_to_indy() {
    let src = "@JvmInline value class Tag(val name: String)\n\
        fun label(f: (Tag) -> String, t: Tag): String = f(t)\n\
        fun box(): String = label({ fun twice(s: String) = s + s; twice(it.name) }, Tag(\"O\"))\n";
    let diagnostics = common::compile_in_process_diagnostics(
        src,
        "NestedLambdaClass",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    );
    assert_eq!(
        diagnostics,
        [
            "internal error: lambda box$lambda$0 needs the class kotlinc writes when \
          LambdaMetafactory cannot adapt its signature, and its shape is not realized as one yet"
        ]
    );
}

/// A generic value class over `T` whose bound admits `null` has a nullable underlying type, which
/// `LambdaMetafactory` adapts, so kotlinc keeps the indy lambda. The class comes from a dependency,
/// where the pass reads its underlying type from the provider rather than from this file.
const GENERIC_LIB: &str = "@JvmInline value class Wrap<T>(val a: T) {\n\
    \x20   fun unwrap(): T = a\n\
    }\n";

const GENERIC_MAIN: &str =
    "fun take(f: (Wrap<String>) -> String, w: Wrap<String>): String = f(w)\n\
    fun box(): String = take({ it.unwrap() }, Wrap(\"OK\"))\n";

#[test]
fn a_dependency_generic_value_class_over_a_nullable_bound_keeps_the_indy_lambda() {
    let library = common::kotlinc_library(GENERIC_LIB).expect("reference kotlinc is provisioned");
    let classes = common::compile_in_process_metadata_cp(
        GENERIC_MAIN,
        "GenericLambda",
        &[library, common::stdlib_jar()],
    )
    .expect("krusty compiles the lambda");
    let names = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["GenericLambdaKt"]);
    assert_eq!(
        common::expect_box_run_against_kotlinc(GENERIC_LIB, GENERIC_MAIN).as_deref(),
        Some("OK")
    );
}
