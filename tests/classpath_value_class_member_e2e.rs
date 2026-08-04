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

/// A classpath member whose RETURN is a value class is JVM-name-MANGLED and physically returns that
/// value class's ERASED underlying (`fun make(): K` → `make-XLNMDGE()Ljava/lang/String;`). The value on
/// the stack is therefore ALREADY the unboxed carrier: tagging the result with the declared `K` as a
/// `checkcast K` and unboxing it hands `unbox-impl` a `String` — a ClassCastException. kotlinc emits
/// neither instruction. The sibling PROPERTY (`val k: K` → `getK-XLNMDGE()`) has the same physical
/// shape and is checked beside it, so the two members stay on one rule.
#[test]
fn classpath_value_class_return_member_is_already_unboxed() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib(
        "vcmemberret",
        "package lib\n\
         @JvmInline value class K(val v: String)\n\
         class Holder {\n\
        \x20   val k: K = K(\"prop\")\n\
        \x20   fun make(): K = K(\"made\")\n\
        \x20   fun echo(x: K): String = x.v\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.Holder\n\
        import lib.K\n\
        fun box(): String {\n\
        \x20 val h = Holder()\n\
        \x20 if (h.make().v != \"made\") return \"f1\"\n\
        \x20 if (h.k.v != \"prop\") return \"f2\"\n\
        \x20 val made: K = h.make()\n\
        \x20 if (made.v != \"made\") return \"f3\"\n\
        \x20 if (h.echo(made) != \"made\") return \"f4\"\n\
        \x20 if (h.echo(h.make()) != \"made\") return \"f5\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to compile a value-class-returning member call");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

/// The carrier rule must NOT swallow a genuine generic slot. Reading a value class back out of a
/// `List<S>` yields a BOX (the list stores boxes), so that read still needs `checkcast S; unbox-impl`
/// even though the member producing it returns the carrier. The two are told apart by the carrier
/// being a CONCRETE type: `S(String)` erases to `Ljava/lang/String;`, which a generic slot's
/// `Ljava/lang/Object;` can never be mistaken for.
///
/// The slot is read into a local first, deliberately. Passing `list[0]` STRAIGHT into a value-class
/// parameter is a separate, pre-existing gap: nothing unboxes a boxed argument at the call itself
/// (the boundary that unboxes is the local's declared type), and it fails the same way with or
/// without the carrier rule.
#[test]
fn a_generic_slot_still_boxes_a_concrete_carrier_value_class() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib(
        "vcgenericslot",
        "package lib\n\
         @JvmInline value class S(val v: String)\n\
         object F {\n\
        \x20   fun make(): S = S(\"a\")\n\
        \x20   fun show(s: S): String = s.v\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.F\n\
        import lib.S\n\
        fun box(): String {\n\
        \x20 val list: List<S> = listOf(F.make())\n\
        \x20 val fromSlot: S = list[0]\n\
        \x20 val direct: S = F.make()\n\
        \x20 return if (F.show(fromSlot) == \"a\" && F.show(direct) == \"a\") \"OK\"\n\
        \x20        else \"fail: ${F.show(fromSlot)}/${F.show(direct)}\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to compile a value class read out of a generic slot");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

/// A value class the callee DECLARES at a parameter position is passed as its erased CARRIER, not as a
/// box. `Result`'s carrier is `Object`, so the mangled member's `invoke-<hash>(Ljava/lang/Object;)`
/// descriptor reads exactly like a generic slot — where kotlinc DOES box — and only the callee's
/// declared signature tells the two apart. Boxing here handed the callee a `Result` object where it
/// expects the carrier, so its own `onFailure` never saw the failure and the call returned the box
/// unchanged: `box()` answered "Fail" with no crash to point at it.
#[test]
fn classpath_value_class_parameter_is_passed_as_its_carrier() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let Some(libout) = common::compile_lib(
        "vccarrier",
        "package lib\n\
         interface UseCaseWithParameter<P, R> {\n\
        \x20   operator fun invoke(param: P): Result<R>\n\
         }\n\
         class WhateverUseCase : UseCaseWithParameter<Result<Int>, Int> {\n\
        \x20   override operator fun invoke(param: Result<Int>): Result<Int> =\n\
        \x20       param.onFailure { return Result.success(0) }\n\
         }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    let main = "import lib.WhateverUseCase\n\
        fun box(): String {\n\
        \x20 val useCase = WhateverUseCase()\n\
        \x20 val recovered = useCase(Result.failure(NumberFormatException()))\n\
        \x20 return if (recovered == Result.success(0)) \"OK\" else \"fail: $recovered\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(jdk.as_path()))
        .expect("krusty failed to compile a value-class-parameter member call");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
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
    let Some(libout) = common::compile_lib(
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
