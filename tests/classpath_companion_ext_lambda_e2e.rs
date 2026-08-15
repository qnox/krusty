//! A lambda passed to a classpath Kotlin MEMBER whose parameter is a RECEIVER function type
//! (`Recv.() -> Unit` — `@Metadata`'s `@ExtensionFunctionType`) binds its implicit `this` to the
//! receiver. Two shapes: an instance member (`holder.build { … }`) and a companion-object member
//! reached through the type name (`FactoryApi.create { … }`, which used to fall through to the
//! unresolved-static recovery path).
//! Needs the JVM toolchain + real kotlinc; skips otherwise.
use super::common;

/// An interface whose companion object declares a factory taking an extension lambda. All names
/// are intentionally fixture-local: the regression is about metadata shape, not a specific
/// dependency or generated runtime class.
const LIB: &str = "package lib\n\
    interface FactoryApi {\n\
        companion object {\n\
            fun create(builderAction: BuildScope.() -> Unit): FactoryApi {\n\
                val b = BuildScope()\n\
                b.builderAction()\n\
                return Product(b.v)\n\
            }\n\
        }\n\
    }\n\
    class BuildScope {\n\
        var v: String = \"\"\n\
    }\n\
    class Product(val v: String) : FactoryApi\n";

/// The companion-object member call resolves and the lambda's implicit `this` binds to the
/// declared receiver (a member assign through `this` type-checks and lowers). Exercise both
/// positional and NAMED binding: expectation lookup must follow semantic parameter slots rather
/// than assuming source argument index equals parameter index.
#[test]
fn classpath_companion_ext_lambda_call() {
    common::expect_box_ok_against(
        "companion_ext_lambda",
        LIB,
        "import lib.FactoryApi\n\
         fun box(): String {\n\
             val positional = FactoryApi.create { v = \"O\" }\n\
             val named = FactoryApi.create(builderAction = { v = \"K\" })\n\
             return (positional as lib.Product).v + (named as lib.Product).v\n\
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
/// SUBSTITUTED parameter type. Also covers REORDERED named lambda arguments and a call with two
/// function-typed parameters (only one of them a receiver type), proving each source argument gets
/// the expectation of the semantic parameter slot it names.
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
             b.mutate(block = { \"O\" + this })\n\
             val combined = b.two(b = { this }, a = { \"O\" })\n\
             return if (b.out == \"OK\" && combined == \"OK\") \"OK\" else \"F:\" + b.out + combined\n\
         }\n",
    );
}

/// A concrete generic receiver must come from the substituted function shape, not the compact
/// metadata classifier. The latter identifies only `List`; retaining `List<String>` is what makes
/// the indexed element a `String` and permits its `length` member.
#[test]
fn classpath_member_parameterized_ext_lambda_receiver() {
    const LIB5: &str = "package lib\n\
        class GenericReceiver {\n\
            fun inspect(block: List<String>.() -> String): String = block(listOf(\"OK\"))\n\
        }\n";
    common::expect_box_ok_against(
        "member_parameterized_ext_lambda",
        LIB5,
        "import lib.GenericReceiver\n\
         fun box(): String {\n\
             val result = GenericReceiver().inspect { if (this[0].length == 2) this[0] else \"F\" }\n\
             return result\n\
         }\n",
    );
}

/// Named lambda arguments to a classpath member with two PLAIN function-typed parameters are a
/// regression control for the same semantic-slot mapping used by the receiver-lambda case above.
/// Reversed source order must preserve each named parameter's position at invocation.
#[test]
fn classpath_member_lambda_named_args() {
    const LIB6: &str = "package lib\n\
        class Pair2 {\n\
            fun two(a: () -> String, b: () -> String): String = a() + b()\n\
        }\n";
    common::expect_box_ok_against(
        "member_lambda_named",
        LIB6,
        "import lib.Pair2\n\
         fun box(): String {\n\
             val named = Pair2().two(b = { \"x\" }, a = { \"y\" })\n\
             return if (named == \"yx\") \"OK\" else \"F:\" + named\n\
         }\n",
    );
}
