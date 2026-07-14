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
