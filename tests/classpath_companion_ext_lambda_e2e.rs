//! A lambda passed to a classpath Kotlin MEMBER whose parameter is a RECEIVER function type
//! (`Recv.() -> Unit` — `@Metadata`'s `@ExtensionFunctionType`) binds its implicit `this` to the
//! receiver. Two shapes: an instance member (`holder.build { … }`) and a companion-object member
//! reached through the type name (`Parameters.build { … }` — the ktor `io.ktor.http.Parameters`
//! shape, which used to fail as "unresolved Java static 'Parameters.build'").
//! Needs the JVM toolchain + real kotlinc; skips otherwise.
use super::common;

/// The ktor `io.ktor.http.Parameters` shape: an interface whose companion object declares a
/// factory taking an extension lambda.
const LIB: &str = "package lib\n\
    interface Parameters {\n\
        companion object {\n\
            fun build(builderAction: ParametersBuilder.() -> Unit): Parameters {\n\
                val b = ParametersBuilder()\n\
                b.builderAction()\n\
                return P(b.v)\n\
            }\n\
        }\n\
    }\n\
    class ParametersBuilder {\n\
        var v: String = \"\"\n\
    }\n\
    class P(val v: String) : Parameters\n";

/// The companion-object member call resolves and the lambda's implicit `this` binds to the
/// declared receiver (a member assign through `this` type-checks and lowers).
#[test]
fn classpath_companion_ext_lambda_call() {
    common::expect_box_ok_against(
        "companion_ext_lambda",
        LIB,
        "import lib.Parameters\n\
         fun box(): String {\n\
             val p = Parameters.build { v = \"OK\" }\n\
             return (p as lib.P).v\n\
         }\n",
    );
}

/// The plain-lambda companion member keeps working (no `@ExtensionFunctionType` involved).
#[test]
fn classpath_companion_plain_lambda_call() {
    const LIB3: &str = "package lib\n\
        interface P2 {\n\
            companion object {\n\
                var seen = \"\"\n\
                fun build(action: () -> Unit): P2 { action(); return object : P2 {} }\n\
            }\n\
        }\n";
    common::expect_box_ok_against(
        "companion_plain_lambda",
        LIB3,
        "import lib.P2\n\
         fun box(): String {\n\
             P2.build { P2.seen = \"OK\" }\n\
             return P2.seen\n\
         }\n",
    );
}

/// Same extension-lambda parameter, three call shapes: instance member, plain-lambda member
/// (control), and a top-level HOF (the previously working shape — a regression control).
#[test]
fn classpath_instance_ext_lambda_call() {
    const LIB2: &str = "package lib\n\
        class Builder { var v: String = \"\" }\n\
        class Holder {\n\
            fun build(builderAction: Builder.() -> Unit): String {\n\
                val b = Builder()\n\
                b.builderAction()\n\
                return b.v\n\
            }\n\
            fun buildPlain(action: () -> Unit): String = \"p\"\n\
        }\n\
        fun runOn(h: Holder, block: Builder.() -> Unit): String = \"t\"\n";
    let Some(diags) = common::diagnostics_against(
        "instance_ext_lambda",
        LIB2,
        "import lib.Holder\n\
         import lib.runOn\n\
         fun box(): String {\n\
             val h = Holder()\n\
             val a = h.build { v = \"x\" }\n\
             val b = h.buildPlain { }\n\
             val c = runOn(h) { v = \"y\" }\n\
             return \"OK\"\n\
         }\n",
    ) else {
        return;
    };
    assert!(diags.is_empty(), "diagnostics: {diags:?}");
}

/// An INSTANCE member call through a run (receiver member assign inside the lambda body).
#[test]
fn classpath_instance_ext_lambda_runs() {
    const LIB2: &str = "package lib\n\
        class Builder { var v: String = \"\" }\n\
        class Holder {\n\
            fun build(builderAction: Builder.() -> Unit): String {\n\
                val b = Builder()\n\
                b.builderAction()\n\
                return b.v\n\
            }\n\
        }\n";
    common::expect_box_ok_against(
        "instance_ext_lambda_run",
        LIB2,
        "import lib.Holder\n\
         fun box(): String = Holder().build { v = \"OK\" }\n",
    );
}

/// A GENERIC receiver function-type parameter (`block: T.() -> String`): `@Metadata` names no
/// receiver class for a type parameter, so the expectation recovers the receiver from the
/// SUBSTITUTED parameter type. Also covers named lambda arguments and a call with two
/// function-typed parameters (only one of them a receiver type).
#[test]
fn classpath_member_generic_ext_lambda_call() {
    const LIB4: &str = "package lib\n\
        class Box<T>(val v: T) {\n\
            var out: String = \"\"\n\
            fun mutate(block: T.() -> String) { out = block(v) }\n\
            fun two(a: () -> String, b: T.() -> String): String = a() + b(v)\n\
        }\n";
    common::expect_box_ok_against(
        "member_generic_ext_lambda",
        LIB4,
        "import lib.Box\n\
         fun box(): String {\n\
             val b = Box(\"K\")\n\
             b.mutate { \"O\" + this }\n\
             return b.out\n\
         }\n",
    );
}

/// Named lambda arguments to a classpath member with two function-typed parameters (regression
/// control: the named-argument slot path types lambdas against the erased parameters — unchanged
/// by this fix). NAMED arguments to a RECEIVER function-type parameter are a pre-existing gap
/// (the slot path has no lambda-expectation channel; it fails on master identically).
#[test]
fn classpath_member_lambda_named_args() {
    const LIB5: &str = "package lib\n\
        class Pair2 {\n\
            fun two(a: () -> String, b: () -> String): String = a() + b()\n\
        }\n";
    common::expect_box_ok_against(
        "member_lambda_named",
        LIB5,
        "import lib.Pair2\n\
         fun box(): String {\n\
             val named = Pair2().two(b = { \"x\" }, a = { \"y\" })\n\
             return if (named == \"yx\") \"OK\" else \"F:\" + named\n\
         }\n",
    );
}
