//! A generic call's erased result that is discarded is popped as it is, as kotlinc does: the
//! `checkcast` or unboxing that reads it as the substituted type exists only for a consumer. A
//! void function's `return <Unit expression>` discards its value the same way.
use super::common;

#[test]
fn a_discarded_generic_result_is_not_cast() {
    let src = "class Box<T>(val v: Any?) { fun get(): T = v as T }\n\
               fun unit(b: Box<Unit>) { b.get() }\n\
               fun reference(b: Box<String>) { b.get() }\n\
               fun primitive(b: Box<Int>) { b.get() }\n\
               fun wide(b: Box<Long>) { b.get() }\n\
               fun nullable(b: Box<Int?>) { b.get() }\n\
               fun returned(b: Box<Unit>): Unit { return b.get() }\n\
               fun branches(b: Box<String>) { if (b.v != null) b.get() else b.get() }\n";
    common::byte_diff_against_kotlinc_cp(
        "DiscardedGenericResult",
        src,
        "DiscardedGenericResultKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("DiscardedGenericResultKt differs from kotlinc:\n{diff}"));
}

/// A lambda's last expression coerced to `Unit` is discarded the same way. Only the lambda bodies
/// are compared: the capture's local variable name is a separate declaration-shape difference.
#[test]
fn a_lambda_discards_its_unit_coerced_generic_result() {
    let src = "class Box<T>(val v: Any?) { fun get(): T = v as T }\n\
               fun <T> run1(f: () -> T): T = f()\n\
               fun lambda(b: Box<Unit>) = run1 { b.get() }\n\
               fun function(b: Box<String>): () -> Unit = { b.get() }\n";
    let pair =
        common::ModuleClassPair::compile(&[("DiscardingLambda.kt", src)], "DiscardingLambdaKt");
    for method in ["lambda$lambda$0", "function$lambda$0"] {
        let (kotlinc, krusty) = pair.method_code("DiscardingLambdaKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

#[test]
fn a_discarded_generic_result_still_runs_its_call() {
    let src = "var calls = 0\n\
               class Box<T>(val v: Any?) { fun get(): T { calls++; return v as T } }\n\
               fun returned(b: Box<Unit>): Unit { return b.get() }\n\
               fun box(): String {\n\
               \x20   Box<String>(1).get()\n\
               \x20   Box<Long>(\"x\").get()\n\
               \x20   returned(Box(Unit))\n\
               \x20   return if (calls == 3) \"OK\" else \"calls $calls\"\n\
               }\n";
    let actual = common::compile_and_run_box(
        src,
        "discarded_generic_result",
        &[common::stdlib_jar()],
        None,
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
