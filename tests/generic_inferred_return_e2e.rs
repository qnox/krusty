//! A SINGLE (non-overloaded) function with a reference-bounded type-parameter param and an inferred
//! (unannotated, expression-body) return. The deeper checker inference must patch the canonical
//! `Signature::ret` for the resolved overload before codegen. The risk is a tparam param:
//! `resolve_ty` erases `T : Number` to its bound, while a key rebuilt from raw AST in codegen
//! (`ty_of`, which erases a bare type parameter to `Object`) would diverge and emit the old
//! `Unit`-defaulted return for a body that returns a `String` (`-Xverify:all` failure). This pins the
//! generic case the same-name-overload test doesn't reach.

use super::common;

#[test]
fn generic_param_inferred_return_keeps_override() {
    let java_home = common::java_home();
    let stdlib = common::stdlib_jar();
    let src = "fun <T : Number> show(x: T) = x.toString()\n\
fun box(): String {\n\
val s = show(7)\n\
if (s != \"7\") return \"fail: \" + s\n\
return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let out = common::compile_and_run_box(src, "G", &[stdlib], Some(&jdk))
        .expect("a generic-param fn with an inferred return must keep that return at codegen");
    assert_eq!(out, "OK");
}

#[test]
fn postponed_generic_lambda_result_finalizes_the_callers_signature() {
    const SOURCE: &str =
        "inline fun <R> apply(value: Int, block: (Int, Int) -> R): R = block(1, value)\n\
        fun inferred(value: Int) = apply(value) { left, right -> (left + right).toString() }\n\
        fun box(): String = inferred(41)\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "PostponedGenericLambda")
            .expect("postponed generic lambda result"),
        "42",
    );
}

// An inferred signature whose body reads through a CLASSIFIER qualifier. A class name in expression
// position denotes its companion instance, and an enum name qualifies its entries; neither is a
// value, so the signature solver used to decline the whole module with no diagnostic at all
// ("module signatures were not finalized"). `fun f() = <expr>` is the only shape that exercises this
// — with an explicit return type the solver never evaluates the body.
#[test]
fn classifier_qualified_reads_infer_a_signature() {
    const COMPANION: &str = "class C { companion object { val x = 1 } }\n\
        fun f() = C.x\n\
        fun box(): String = if (f() == 1) \"OK\" else \"Fail\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(COMPANION, "Main").expect("companion property"),
        "OK"
    );

    const ENUM_ENTRY: &str = "enum class E { OK }\n\
        fun f() = E.OK\n\
        fun box(): String = f().toString()\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(ENUM_ENTRY, "Main").expect("enum entry"),
        "OK"
    );

    const ENUM_ENTRY_MEMBER_CALL: &str = "enum class E { OK }\n\
        fun box() = E.OK.toString()\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(ENUM_ENTRY_MEMBER_CALL, "Main")
            .expect("enum entry member call"),
        "OK"
    );

    const OBJECT_PROPERTY: &str = "object O { val x = 1 }\n\
        fun f() = O.x\n\
        fun box(): String = if (f() == 1) \"OK\" else \"Fail\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(OBJECT_PROPERTY, "Main").expect("object property"),
        "OK"
    );
}

// A CONSTRUCTOR reference used as a plain function value. `::A` names a class, not a function, so
// signature inference has to build the reference type from the constructor's parameters, and checked
// FIR has to materialize an adapter that CONSTRUCTS rather than calls.
#[test]
fn constructor_reference_infers_and_constructs() {
    const AS_FUNCTION_VALUE: &str = "class A(val result: String)\n\
        fun apply1(f: (String) -> A): String = f(\"OK\").result\n\
        fun box(): String = apply1(::A)\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(AS_FUNCTION_VALUE, "Main")
            .expect("constructor reference as a function value"),
        "OK"
    );

    const INFERRED_DECLARATION: &str = "class A(val result: String)\n\
        fun f() = ::A\n\
        fun box(): String = f()(\"OK\").result\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(INFERRED_DECLARATION, "Main")
            .expect("inferred constructor reference"),
        "OK"
    );
}

// A member property INITIALIZER runs inside the constructor, where the primary-constructor
// parameters are ordinary locals — which is exactly how the checker types them. The checked body
// unit has to carry them, or a name the checker resolved has no binding in FIR.
#[test]
fn property_initializers_read_constructor_parameters() {
    const VAL_PARAM: &str = "class A(val y: Int) { var x = y }\n\
        fun box(): String = if (A(42).x == 42) \"OK\" else \"Fail\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(VAL_PARAM, "Main").expect("val ctor property"),
        "OK"
    );

    const PLAIN_PARAM: &str = "class A(y: Int) { var x = y }\n\
        fun box(): String = if (A(42).x == 42) \"OK\" else \"Fail\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(PLAIN_PARAM, "Main").expect("plain ctor parameter"),
        "OK"
    );

    const EXPLICIT_TYPE: &str = "class A(y: Int) { var x: Int = y }\n\
        fun box(): String = if (A(42).x == 42) \"OK\" else \"Fail\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(EXPLICIT_TYPE, "Main").expect("explicit initializer"),
        "OK"
    );

    // The sibling-property read that used to report a spurious recursive-inference error, because
    // constructor and body properties collided in one `SourceMember::ClassProperty` index space.
    const INFERRED_GETTER: &str = "class A(val y: Int) { val x get() = y }\n\
        fun box(): String = if (A(42).x == 42) \"OK\" else \"Fail\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(INFERRED_GETTER, "Main").expect("inferred getter"),
        "OK"
    );
}

#[test]
fn generic_extension_getter_resolves_its_compact_type_parameter() {
    const SRC: &str = "val <T> T.echo get() = this as T\n\
fun box(): String = \"OK\".echo\n";
    common::expect_box_ok_with_stdlib(SRC, "GenericExtensionGetter");
}

#[test]
fn generic_inferred_extension_getter_metadata_is_consumable() {
    const LIBRARY: &str = "package dep\nval <T> T.echo get() = this as T\n";
    const CONSUMER: &str = "import dep.echo\nfun box(): String = \"OK\".echo\n";
    assert_eq!(
        common::run_box_against(
            "generic_inferred_extension_getter_metadata",
            LIBRARY,
            CONSUMER,
        )
        .as_deref(),
        Some("OK")
    );
}

#[test]
fn generic_member_extension_getter_resolves_its_compact_type_parameter() {
    const SRC: &str = "class Scope { val <T> T.echo get() = this as T }\n\
fun box(): String = with(Scope()) { \"OK\".echo }\n";
    common::expect_box_ok_with_stdlib(SRC, "GenericMemberExtensionGetter");
}

#[test]
fn generic_inferred_member_extension_getter_metadata_is_consumable() {
    const LIBRARY: &str = "package dep\nclass Scope { val <T> T.echo get() = this as T }\n";
    const CONSUMER: &str = "import dep.Scope\nfun box(): String = with(Scope()) { \"OK\".echo }\n";
    common::expect_box_ok_against(
        "generic_inferred_member_extension_getter_metadata",
        LIBRARY,
        CONSUMER,
    );
}

// A deferred `val` assigned from an `init` block. The checker commits the owner and type as a
// `DeferredPropertyWrite`; checked FIR consumes that decision as a direct backing-field store rather
// than looking for a local of that name.
#[test]
fn deferred_val_assigned_from_an_init_block() {
    const CLASS: &str = "class Foo {\n\
        \x20 val bar: String\n\
        \x20 init { bar = \"OK\" }\n\
        }\n\
        fun box(): String = Foo().bar\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(CLASS, "Main").expect("deferred val in a class"),
        "OK"
    );

    const OBJECT: &str = "object Foo {\n\
        \x20 val bar: String\n\
        \x20 init { bar = \"OK\" }\n\
        }\n\
        fun box(): String = Foo.bar\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(OBJECT, "Main").expect("deferred val in an object"),
        "OK"
    );
}
