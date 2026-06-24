//! The metadata-primary function reader (`crate::jvm::metadata`) must surface the *Kotlin* signature of
//! `inline` functions — which are `private`/synthetic in bytecode, so their public API exists only in
//! `@Metadata`. Validated against the real stdlib `kotlin.Result`, whose `Companion.success`/`failure`
//! and the `ResultKt` extensions (`getOrThrow`, …) are all public `inline`.

mod common;

use krusty::jvm::classpath::Classpath;
use krusty::jvm::metadata::{
    class_companion_name, class_functions, class_inline, package_functions,
};

fn cp() -> Option<Classpath> {
    let sl = common::stdlib_jar()?;
    Some(Classpath::new(vec![sl]))
}

#[test]
fn result_companion_success_is_public_inline_in_metadata() {
    let Some(cp) = cp() else {
        eprintln!("no stdlib jar; skipping");
        return;
    };
    // `Result` declares a companion named `Companion`.
    let result_ci = cp
        .find("kotlin/Result")
        .expect("kotlin/Result on classpath");
    assert_eq!(
        class_companion_name(&result_ci).as_deref(),
        Some("Companion")
    );

    // `Result.Companion.success`/`failure` — public inline per @Metadata, though private in bytecode.
    let comp_ci = cp
        .find("kotlin/Result$Companion")
        .expect("kotlin/Result$Companion on classpath");
    let fns = class_functions(&comp_ci);
    let success = fns
        .iter()
        .find(|f| f.kotlin_name == "success")
        .expect("Companion.success in metadata");
    assert!(success.is_public, "success must be public in metadata");
    assert!(success.is_inline, "success must be inline");
    assert_eq!(success.jvm_name, "success");
    assert_eq!(
        success.jvm_desc.as_deref(),
        Some("(Ljava/lang/Object;)Ljava/lang/Object;")
    );
    assert!(fns.iter().any(|f| f.kotlin_name == "failure"));
}

#[test]
fn resultkt_get_or_throw_is_public_inline_extension_in_metadata() {
    let Some(cp) = cp() else {
        eprintln!("no stdlib jar; skipping");
        return;
    };
    let kt = cp
        .find("kotlin/ResultKt")
        .expect("kotlin/ResultKt on classpath");
    let fns = package_functions(&kt);
    let g = fns
        .iter()
        .find(|f| f.kotlin_name == "getOrThrow")
        .expect("getOrThrow in ResultKt metadata");
    assert!(g.is_public, "getOrThrow must be public in metadata");
    assert!(g.is_inline, "getOrThrow must be inline");
    assert_eq!(
        g.receiver_class.as_deref(),
        Some("kotlin/Result"),
        "getOrThrow is an extension on Result"
    );
}

#[test]
fn inline_class_underlying_types_from_metadata() {
    let Some(cp) = cp() else {
        eprintln!("no stdlib jar; skipping");
        return;
    };
    // `value class Result<T>(val value: Any?)` — underlying is `Any?` → erases to Object.
    let result_ci = cp
        .find("kotlin/Result")
        .expect("kotlin/Result on classpath");
    let ic = class_inline(&result_ci).expect("Result is a @JvmInline value class");
    assert_eq!(ic.underlying_class.as_deref(), Some("kotlin/Any"));

    // `UInt` is a value class over `kotlin/Int`.
    let uint_ci = cp.find("kotlin/UInt").expect("kotlin/UInt on classpath");
    let ic = class_inline(&uint_ci).expect("UInt is a @JvmInline value class");
    assert_eq!(ic.underlying_class.as_deref(), Some("kotlin/Int"));

    // An ordinary class is not a value class.
    let pair_ci = cp.find("kotlin/Pair").expect("kotlin/Pair on classpath");
    assert!(
        class_inline(&pair_ci).is_none(),
        "Pair is not a value class"
    );
}
