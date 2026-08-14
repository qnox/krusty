//! e2: a classpath interface/class METHOD whose parameter is a value class is JVM-name-MANGLED
//! (`fun get(id: Vid): Cat` → `get-<hash>(String)`). Resolving it by source name `get` must recover the
//! mangled JVM name + the logical `Vid` parameter type from `@Metadata`, and the call must pass the
//! unboxed underlying — exactly kotlinc's `invokeinterface Port.get-<hash>(String)`.
//! Needs the JVM toolchain + kotlin-stdlib; skips otherwise.
use super::common;

#[test]
fn classpath_value_class_param_member_resolves_mangled() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    // A classpath library: a value class, an interface with a value-class-param method, and a factory so
    // the box() can obtain a `Port` without implementing the mangled method itself.
    let Some(libout) = common::compile_lib(
        "vcmember",
        "package lib\n\
         @JvmInline value class Vid(val v: String)\n\
         class Cat(val name: String)\n\
         interface Port { fun get(id: Vid): Cat }\n\
         private class PortImpl : Port { override fun get(id: Vid): Cat = Cat(\"cat-\" + id.v) }\n\
         fun makePort(): Port = PortImpl()\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.makePort\n\
        import lib.Vid\n\
        fun box(): String {\n\
        \x20 val p = makePort()\n\
        \x20 val c = p.get(Vid(\"7\"))\n\
        \x20 return if (c.name == \"cat-7\") \"OK\" else \"fail: ${c.name}\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to compile value-class-param member call");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

/// Operator invocation must carry the selected declaration's return fact just like an explicitly
/// named member call. The generic value class below erases its carrier to `Object`, so logical and
/// physical types cannot classify the result: a real box read from a generic slot has the identical
/// JVM shape. Only `Factory.invoke`'s declared, pre-substitution return proves that the `String` left
/// by the mangled method is the unboxed `TokenBox<String>` carrier. Without that fact the consumer
/// emits `checkcast TokenBox; unbox-impl` over the `String` and fails at runtime.
#[test]
fn operator_invoke_uses_declared_value_class_return() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(libout) = common::compile_lib_ref(
        "vcoperatorreturn",
        "package lib\n\
         @JvmInline value class TokenBox<T>(val value: T)\n\
         class Factory { operator fun invoke(): TokenBox<String> = TokenBox(\"OK\") }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), stdlib.clone()];
    let main = "import lib.Factory\nfun box(): String = Factory()().value\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to compile an operator with a declared value-class return");
    match common::run_box(&classes, "MainKt", &[libout, stdlib]) {
        Some(output) => assert_eq!(output.trim(), "OK", "box() = {output:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

/// A COMPUTED member property of a classpath `@JvmInline value class` is realized as a STATIC
/// `-impl` accessor whose sole parameter is the receiver's carrier
/// (`val isFreezing: Boolean` → `isFreezing-impl(I)Z`, `val label: String` → `getLabel-impl(I)`).
/// The class's own underlying property keeps an ORDINARY instance getter (`getDegrees()I`) because
/// it IS the carrier. Both spellings must read, and the static one must consume the receiver as its
/// carrier argument rather than evaluating it for effect and invoking with an empty stack.
#[test]
fn classpath_value_class_member_property_reads_through_impl_accessor() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib_ref(
        "vcmemberprop",
        "package lib\n\
         @JvmInline value class Celsius(val degrees: Int) {\n\
        \x20   val isFreezing: Boolean get() = degrees <= 0\n\
        \x20   val label: String get() = \"\" + degrees + \" C\"\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Celsius\n\
        fun box(): String {\n\
        \x20 val cold = Celsius(-5)\n\
        \x20 if (!cold.isFreezing) return \"f1\"\n\
        \x20 if (cold.degrees != -5) return \"f2\"\n\
        \x20 if (cold.label != \"-5 C\") return \"f3: ${cold.label}\"\n\
        \x20 if (Celsius(20).isFreezing) return \"f4\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to compile value-class member property reads");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
