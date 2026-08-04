//! A classifier and a same-named top-level factory function sharing one name (the
//! `fun UnscaledGapsY(...): UnscaledGapsY` idiom): when no constructor is applicable — an
//! interface has none at all — kotlinc binds the call to the function. Constructor candidates
//! keep precedence whenever one DOES apply (kotlinc: an applicable abstract-class constructor
//! still errors, and a class's applicable constructor beats the function).

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
}

/// The BannerStartPagePromoter case: `UnscaledGapsY(top = 4, bottom = 8)` where the interface
/// and its top-level factory share a name.
#[test]
fn named_args_pick_factory() {
    run_ok(
        "IfaceFactoryNamed",
        "interface GapsY { val top: Int; val bottom: Int }\n\
         fun GapsY(top: Int = 0, bottom: Int = 0): GapsY = GapsYImpl(top, bottom)\n\
         private class GapsYImpl(val t: Int, val b: Int) : GapsY {\n\
         \x20   override val top: Int get() = t\n\
         \x20   override val bottom: Int get() = b\n\
         }\n\
         fun box(): String {\n\
         \x20   val g = GapsY(top = 4, bottom = 8)\n\
         \x20   return if (g.top == 4 && g.bottom == 8) \"OK\" else \"F\" }\n",
    );
}

/// `GapsY()` (all defaults) and `GapsY(4)` (positional) both pick the factory — an interface
/// has no constructor candidate at all, however the call is shaped.
#[test]
fn positional_and_default_args_pick_factory() {
    run_ok(
        "IfaceFactoryPositional",
        "interface GapsY { val top: Int; val bottom: Int }\n\
         fun GapsY(top: Int = 0, bottom: Int = 0): GapsY = GapsYImpl(top, bottom)\n\
         private class GapsYImpl(val t: Int, val b: Int) : GapsY {\n\
         \x20   override val top: Int get() = t\n\
         \x20   override val bottom: Int get() = b\n\
         }\n\
         fun box(): String {\n\
         \x20   val a = GapsY(4)\n\
         \x20   val b = GapsY()\n\
         \x20   return if (a.top == 4 && b.top == 0 && b.bottom == 0) \"OK\" else \"F\" }\n",
    );
}

/// The real declaration also carries a companion object; the factory still wins over the
/// classifier.
#[test]
fn factory_wins_over_companion_object() {
    run_ok(
        "IfaceFactoryCompanion",
        "interface GapsY {\n\
         \x20   companion object { @JvmField val EMPTY: GapsY = EmptyGapsY }\n\
         \x20   val top: Int\n\
         }\n\
         fun GapsY(top: Int = 0): GapsY = GapsYImpl(top)\n\
         private object EmptyGapsY : GapsY { override val top: Int = 0 }\n\
         private class GapsYImpl(val t: Int) : GapsY { override val top: Int get() = t }\n\
         fun box(): String {\n\
         \x20   val g = GapsY(top = 4)\n\
         \x20   return if (g.top == 4 && GapsY.EMPTY.top == 0) \"OK\" else \"F\" }\n",
    );
}

/// An interface WITHOUT a factory function stays an error, with the message unchanged.
#[test]
fn interface_without_factory_still_rejected() {
    let d = diags("interface NoFac { val x: Int }\nfun f() { val g = NoFac() }");
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        d.iter()
            .any(|m| m.contains("cannot create an instance of an interface 'NoFac'")),
        "expected the interface-instantiation diagnostic, got: {d:?}"
    );
}

/// kotlinc prefers the constructor when BOTH it and a same-named function apply: `Abs()` binds
/// the (abstract) constructor and errors, never the default-arg factory.
#[test]
fn abstract_class_applicable_ctor_still_rejected() {
    let d = diags(
        "abstract class Abs { abstract val x: Int }\n\
         fun Abs(x: Int = 1): Abs = AbsImpl(x)\n\
         private class AbsImpl(val v: Int) : Abs() { override val x: Int get() = v }\n\
         fun f() { val a = Abs() }",
    );
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        d.iter()
            .any(|m| m.contains("cannot create an instance of an abstract class 'Abs'")),
        "expected the abstract-instantiation diagnostic, got: {d:?}"
    );
}

/// A class whose constructor does NOT apply falls through to the same-named factory (kotlinc
/// binds `C("ab")` to the function); applicable constructor calls keep constructing.
#[test]
fn constructor_preferred_when_applicable() {
    run_ok(
        "CtorPreferredOverFactory",
        "class C(val x: Int = 0)\n\
         fun C(s: String): C = C(s.length)\n\
         fun box(): String {\n\
         \x20   val a = C(1)\n\
         \x20   val b = C(\"ab\")\n\
         \x20   val c = C()\n\
         \x20   return if (a.x == 1 && b.x == 2 && c.x == 0) \"OK\" else \"F\" }\n",
    );
}

/// An abstract class's constructor takes no `x`, so `Abs(5)` / `Abs(x = 7)` bind the factory
/// (kotlinc accepts exactly these shapes).
#[test]
fn abstract_class_factory() {
    run_ok(
        "AbstractFactory",
        "abstract class Abs { abstract val x: Int }\n\
         fun Abs(x: Int = 1): Abs = AbsImpl(x)\n\
         private class AbsImpl(val v: Int) : Abs() { override val x: Int get() = v }\n\
         fun box(): String = if (Abs(5).x == 5 && Abs(x = 7).x == 7) \"OK\" else \"F\"\n",
    );
}
