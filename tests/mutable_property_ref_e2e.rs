//! Bound and unbound MUTABLE (`var`) property references (`obj::v`, `Type::v`). The lowerer used to
//! bail (`if is_var { return None }`) and hardcode the immutable `PropertyReference*Impl`; it now picks
//! the `KMutableProperty*` runtime class and the emitters add a `set` method (`obj::v` →
//! `((Owner) receiver).setV(x)`, `Type::v` → `((Owner) it).setV(x)`) so `ref.set(x)` writes back.
use super::common;

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    common::compile_and_run_box(main, "Main", &[sl, jdk.clone()], Some(&jdk))
}

#[test]
fn immutable_bound_property_ref_invoke() {
    const MAIN: &str = "class A(val v: Int)\n\
        fun box(): String {\n\
        \x20 val a = A(5)\n\
        \x20 val av = a::v\n\
        \x20 return if (av() == 5) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("immutable bound property ref invoke"),
        "OK"
    );
}

#[test]
fn immutable_bound_property_ref_get() {
    const MAIN: &str = "class A(val v: Int)\n\
        fun box(): String {\n\
        \x20 val a = A(5)\n\
        \x20 val av = a::v\n\
        \x20 return if (av.get() == 5) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("immutable bound property ref get"), "OK");
}

#[test]
fn bound_mutable_property_ref_get_and_set() {
    // `a::v` is a KMutableProperty0: invoke/get read, set writes back through the captured receiver.
    const MAIN: &str = "class A(var v: Int)\n\
        fun box(): String {\n\
        \x20 val a = A(5)\n\
        \x20 val av = a::v\n\
        \x20 if (av() != 5) return \"fail-invoke: ${av()}\"\n\
        \x20 if (av.get() != 5) return \"fail-get: ${av.get()}\"\n\
        \x20 av.set(7)\n\
        \x20 if (a.v != 7) return \"fail-set: ${a.v}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("bound mutable property ref"), "OK");
}

#[test]
fn unbound_mutable_property_ref_get_and_set() {
    // `A::v` is a KMutableProperty1: get(receiver) reads, set(receiver, value) writes.
    const MAIN: &str = "class A(var v: Int)\n\
        fun box(): String {\n\
        \x20 val p = A::v\n\
        \x20 val a = A(5)\n\
        \x20 if (p.get(a) != 5) return \"fail-get: ${p.get(a)}\"\n\
        \x20 p.set(a, 9)\n\
        \x20 if (a.v != 9) return \"fail-set: ${a.v}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("unbound mutable property ref"), "OK");
}

#[test]
fn unbound_mutable_property_ref_on_protected() {
    // A `protected var` reference works (protected is not the blocker; a name clash is).
    const MAIN: &str = "class Foo {\n\
        \x20 protected var x = 0\n\
        \x20 fun ref() = Foo::x\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = Foo().ref()\n\
        \x20 val foo = Foo()\n\
        \x20 r.set(foo, 42)\n\
        \x20 return if (r.get(foo) == 42) \"OK\" else \"Fail\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("protected property ref"), "OK");
}

#[test]
fn property_ref_declines_on_accessor_name_clash() {
    // A user `fun getX()` collides with the `var x` accessor `getX()`; the ref would dispatch to a
    // `getX()` that isn't reliably emitted, so the reference is DECLINED (the file skips) rather than
    // miscompiled into a NoSuchMethodError.
    const MAIN: &str = "class Foo {\n\
        \x20 var x = 0\n\
        \x20 fun getX() = Foo::x\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = Foo().getX()\n\
        \x20 val foo = Foo()\n\
        \x20 r.set(foo, 42)\n\
        \x20 return if (r.get(foo) == 42) \"OK\" else \"Fail\"\n\
        }\n";
    assert!(
        run(MAIN).is_none(),
        "accessor name clash must decline (skip), not miscompile"
    );
}

#[test]
fn bound_mutable_property_ref_long() {
    // A 2-slot (Long) property exercises the setter's argument stack sizing.
    const MAIN: &str = "class A(var v: Long)\n\
        fun box(): String {\n\
        \x20 val a = A(5L)\n\
        \x20 val av = a::v\n\
        \x20 av.set(7L)\n\
        \x20 return if (a.v == 7L && av.get() == 7L) \"OK\" else \"fail: ${a.v}\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("long mutable property ref"), "OK");
}

#[test]
fn bound_mutable_property_ref_reference_type() {
    // A reference-typed mutable property (the set path casts the erased Object arg, no unbox).
    const MAIN: &str = "class A(var s: String)\n\
        fun box(): String {\n\
        \x20 val a = A(\"x\")\n\
        \x20 val r = a::s\n\
        \x20 r.set(\"OK\")\n\
        \x20 return a.s\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("reference-typed mutable property ref"),
        "OK"
    );
}

#[test]
fn bound_extension_function_ref() {
    const MAIN: &str = "class A(val v: Int)\n\
        fun A.g(x: Int) = x * v\n\
        fun box(): String {\n\
        \x20 val a = A(5)\n\
        \x20 val ag = a::g\n\
        \x20 return if (ag(10) == 50) \"OK\" else \"fail: ${ag(10)}\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("bound extension function ref"), "OK");
}

#[test]
fn unbound_extension_function_ref() {
    const MAIN: &str = "class A(val v: Int)\n\
        fun A.g(x: Int) = x * v\n\
        fun box(): String {\n\
        \x20 val ag = A::g\n\
        \x20 val a = A(5)\n\
        \x20 return if (ag(a, 10) == 50) \"OK\" else \"fail: ${ag(a, 10)}\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("unbound extension function ref"), "OK");
}

#[test]
fn bound_extension_property_ref() {
    // `a::w` where `val A.w` is an extension property → KProperty0 dispatching the static ext getter.
    const MAIN: &str = "class A(val v: Int)\n\
        val A.w: Int get() = 1000 * v\n\
        fun box(): String {\n\
        \x20 val a = A(5)\n\
        \x20 val aw = a::w\n\
        \x20 return if (aw() == 5000 && aw.get() == 5000) \"OK\" else \"fail: ${aw()}\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("bound extension property ref"), "OK");
}

#[test]
fn bound_mutable_extension_property_ref() {
    const MAIN: &str = "class A(var v: Int)\n\
        var A.w: Int\n\
        \x20 get() = v\n\
        \x20 set(x) { v = x }\n\
        fun box(): String {\n\
        \x20 val a = A(5)\n\
        \x20 val aw = a::w\n\
        \x20 aw.set(9)\n\
        \x20 return if (a.v == 9 && aw.get() == 9) \"OK\" else \"fail: ${a.v}\"\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("bound mutable extension property ref"),
        "OK"
    );
}

#[test]
fn object_property_ref() {
    // `F::u` where F is an object → KProperty0 bound to F.INSTANCE.
    const MAIN: &str = "object F { var u = 0 }\n\
        fun box(): String {\n\
        \x20 val fu = F::u\n\
        \x20 if (fu() != 0) return \"fail-invoke\"\n\
        \x20 fu.set(8)\n\
        \x20 return if (F.u == 8 && fu.get() == 8) \"OK\" else \"fail: ${F.u}\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("object property ref"), "OK");
}

#[test]
fn inherited_member_property_ref() {
    // `b::f` where f is a `var` on superclass A (inherited) — get/set dispatch through the inherited
    // accessor on the receiver.
    const MAIN: &str = "open class A { var f: String = \"x\" }\n\
        class B : A()\n\
        fun box(): String {\n\
        \x20 val b = B()\n\
        \x20 val r = b::f\n\
        \x20 r.set(\"OK\")\n\
        \x20 return if (r.get() == \"OK\" && b.f == \"OK\") r.get() else \"fail\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("inherited member property ref"), "OK");
}

#[test]
fn unbound_object_method_refs() {
    // A::equals / A::hashCode / A::toString reference java/lang/Object methods.
    const MAIN: &str = "class A\n\
        fun box(): String {\n\
        \x20 val a = A()\n\
        \x20 val eq = A::equals\n\
        \x20 if (eq(a, a) != true) return \"fail-eq-same\"\n\
        \x20 if (eq(a, A()) != false) return \"fail-eq-diff\"\n\
        \x20 val hc = A::hashCode\n\
        \x20 if (hc(a) != a.hashCode()) return \"fail-hc\"\n\
        \x20 val ts = A::toString\n\
        \x20 if (ts(a) != a.toString()) return \"fail-ts\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("unbound object method refs"), "OK");
}

#[test]
fn bound_object_method_ref() {
    // obj::toString references Object.toString bound to the captured receiver (dispatches an override).
    const MAIN: &str = "class A { override fun toString() = \"OK\" }\n\
        fun box(): String {\n\
        \x20 val a = A()\n\
        \x20 val ts = a::toString\n\
        \x20 return ts()\n\
        }\n";
    assert_eq!(run(MAIN).expect("bound object method ref"), "OK");
}

#[test]
fn nullable_typeparam_tostring_ref_declines() {
    // t::toString where t: T may be null needs kotlinc's null-safe intrinsic; krusty declines (skip)
    // rather than NPE on the captured null.
    const MAIN: &str = "fun <T> get(t: T): () -> String = t::toString\n\
        fun box(): String {\n\
        \x20 if (get(null).invoke() != \"null\") return \"Fail null\"\n\
        \x20 return get(\"OK\").invoke()\n\
        }\n";
    assert!(
        run(MAIN).is_none(),
        "nullable-typeparam toString ref must decline, not NPE"
    );
}

#[test]
fn generic_toplevel_function_ref() {
    // ::id where fun <T> id(x: T): T — the generic type param erases to Object in the lifted static.
    const MAIN: &str = "fun <T> id(x: T): T = x\n\
        fun box(): String = \"OK\".let(::id)\n";
    assert_eq!(run(MAIN).expect("generic top-level function ref"), "OK");
}
