//! ee1: a call to a classpath EXTENSION whose value parameter is a VALUE CLASS (`inline fun <reified T>
//! Reg.getFor(id: Id): T`, where `Id` is `@JvmInline`) failed with "unresolved method 'getFor' on 'lib/
//! Reg'". The value-class parameter mangles the extension's JVM name (`getFor-<hash>`) and erases the
//! parameter to its underlying, so the literal-name extension index missed it, AND the erased signature made
//! the argument (`Id`) fail to match the (underlying `String`) parameter.
//!
//! The extension query now maps `getFor` → the mangled `jvm_name` via `@Metadata` (extension receiver ==
//! the receiver, at least one value-class value parameter) and exposes it with LOGICAL value-class parameter
//! types, and `bound_logical_params` prefers the value-class type over the erased-underlying signature so the
//! value-class argument matches. (The reified inline extension itself must be inlined at each call site —
//! krusty cannot splice a reified body from its throwing bytecode stub, so the CALL lowers to a clean skip
//! rather than a direct invocation; this test asserts the RESOLUTION, the reported failure.)
use super::common;

#[test]
fn valueclass_param_reified_extension_resolves() {
    const LIB: &str = "package lib\n\
        @JvmInline value class Id(val v: String)\n\
        class Prov\n\
        class Reg { fun getFor(id: Id): Any = TODO() }\n\
        inline fun <reified T> Reg.getFor(id: Id): T = TODO()\n";
    // Explicit type argument — resolves to the reified extension (the member is not generic).
    let Some(diags) = common::checker_diags_against(
        "ee1_expl",
        LIB,
        "import lib.Reg\nimport lib.Id\nimport lib.Prov\n\
         fun f(r: Reg, id: Id): Prov = r.getFor<Prov>(id)\nfun box(): String = \"OK\"\n",
    ) else {
        return;
    };
    assert_eq!(
        diags,
        Vec::<String>::new(),
        "value-class-param extension getFor must resolve (was 'unresolved method')"
    );
}

#[test]
fn valueclass_param_extension_no_explicit_type_arg() {
    // Without the explicit type argument, the same value-class-param extension still resolves.
    const LIB: &str = "package lib\n\
        @JvmInline value class Id(val v: String)\n\
        class Reg { fun tag(id: Id): String = id.v }\n\
        inline fun Reg.mk(id: Id): String = tag(id)\n";
    let Some(diags) = common::checker_diags_against(
        "ee1_noexpl",
        LIB,
        "import lib.Reg\nimport lib.Id\n\
         fun f(r: Reg, id: Id): String = r.mk(id)\nfun box(): String = \"OK\"\n",
    ) else {
        return;
    };
    assert_eq!(
        diags,
        Vec::<String>::new(),
        "value-class-param extension mk must resolve"
    );
}
