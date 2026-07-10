//! The metadata-primary function reader (`crate::jvm::metadata`) must surface the *Kotlin* signature of
//! `inline` functions — which are `private`/synthetic in bytecode, so their public API exists only in
//! `@Metadata`. Validated against the real stdlib `kotlin.Result`, whose `Companion.success`/`failure`
//! and the `ResultKt` extensions (`getOrThrow`, …) are all public `inline`.

use super::common;

use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::metadata::{
    class_companion_name, class_functions, class_inline, package_functions,
};
use krusty::symbol_resolver::SymbolResolver;
use krusty::types::Ty;
use std::rc::Rc;

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
        success.jvm_desc,
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
        g.receiver_class,
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

#[test]
fn result_get_or_throw_resolves_as_inline_extension() {
    // Metadata-primary extension resolution: `getOrThrow` is PRIVATE in bytecode (it's `inline`) but
    // PUBLIC per @Metadata with an extension receiver of `kotlin/Result`. It must resolve as an inline
    // extension on a `Result` receiver — found at the erased `Object` rung, disambiguated by the
    // @Metadata receiver. (Byte-equal codegen additionally needs value-class param erasure.)
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("no stdlib jar; skipping");
        return;
    };
    let libs = JvmLibraries::new(Rc::new(Classpath::new(vec![sl])));
    // `getOrThrow` lives in `kotlin/ResultKt` (package `kotlin`); an unqualified extension resolves only
    // through the import scope, so put `kotlin` in scope (matching a file that has `Result` in scope).
    let scope = vec!["kotlin".to_string()];
    let resolver = SymbolResolver::new_scoped(&libs, &scope);
    let c = resolver
        .resolve_extension_inline_callable("getOrThrow", Ty::obj("kotlin/Result"), &[])
        .expect("getOrThrow resolves on a Result receiver via @Metadata");
    assert_eq!(c.owner, "kotlin/ResultKt");
    assert_eq!(c.name, "getOrThrow");
    assert!(c.inline.can_inline(), "getOrThrow is inline");

    // The same name must NOT resolve on an unrelated receiver (the erased-Object candidate is gated by
    // the @Metadata receiver class).
    assert!(
        resolver
            .resolve_extension_inline_callable("getOrThrow", Ty::obj("kotlin/String"), &[])
            .is_none(),
        "getOrThrow must not bind a non-Result receiver"
    );
}

#[test]
fn result_get_or_null_resolves_as_nullable_metadata_member() {
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("no stdlib jar; skipping");
        return;
    };
    let libs = JvmLibraries::new(Rc::new(Classpath::new(vec![sl])));
    let m = krusty::symbol_resolver::resolve_instance_member(
        &libs,
        Ty::obj("kotlin/Result"),
        "getOrNull",
        &[],
    )
    .expect("getOrNull resolves on Result via value-class @Metadata");
    assert_eq!(m.member.owner.as_deref(), Some("kotlin/Result"));
    assert_eq!(m.member.name, "getOrNull-impl");
    assert_eq!(
        m.member.descriptor,
        "(Ljava/lang/Object;)Ljava/lang/Object;"
    );
    assert_eq!(m.member.physical_ret, Ty::obj("kotlin/Any"));
    assert!(
        m.ret.is_nullable(),
        "getOrNull returns nullable T? at the Kotlin boundary"
    );
}

#[test]
fn metadata_decodes_value_parameter_names() {
    let Some(cp) = cp() else {
        eprintln!("no stdlib jar; skipping");
        return;
    };
    // `@Metadata` carries each SOURCE value parameter's NAME (proto `ValueParameter.name`), which
    // named-argument resolution of a classpath call needs. `kotlin.Pair.copy(first, second)` is a stable
    // public, non-inline target.
    let pair = cp.find("kotlin/Pair").expect("kotlin/Pair on classpath");
    let fns = class_functions(&pair);
    let copy = fns
        .iter()
        .find(|f| f.kotlin_name == "copy")
        .expect("Pair.copy in metadata");
    assert_eq!(
        copy.value_params
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>(),
        vec!["first".to_string(), "second".to_string()],
        "copy's value-parameter names decode from @Metadata"
    );
    // Value-parameter facts are one record per source parameter.
    assert_eq!(copy.value_params.len(), 2);
    let equals = fns.iter().find(|f| f.kotlin_name == "equals").unwrap();
    assert_eq!(equals.value_params[0].name, "other");
}
