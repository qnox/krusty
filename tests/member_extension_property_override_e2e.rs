//! A member-extension property is a property whose accessors take the extension receiver
//! (`getX(receiver)`, `setX(receiver, value)`), and it overrides and is delegated like any other.
//!
//! Its override slot includes the receiver: `val T.x` in `G<T>` is overridden by `val String.x` in
//! `G<String>`, which needs kotlinc's erased `getX(Object)` bridge. A class delegating an interface
//! (`class D : G<S> by impl`) forwards the accessors, and a value-class receiver keeps the
//! value-class ABI on every side: the mangled accessor, the generic bridge that unboxes into it,
//! and the box a caller passes through the generic interface.

use super::common;

/// The bridge's instructions match kotlinc's, over an interface kotlinc compiled. Whole-class
/// identity is not asserted: it also covers the accessors' own debug tables and metadata flags.
fn assert_same_method(name: &str, lib: &str, src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc_lib(name, lib, src, class, method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{name}: reference toolchain unavailable"),
    }
}

fn assert_box_ok(stem: &str, src: &str) {
    let output = common::compile_and_run_box(src, stem, &[common::stdlib_jar()], None)
        .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}

/// A generic interface's `val T.x` / `var T.y` are overridden over `String`: the implementation
/// carries the erased `getX(Object)`, `getY(Object)` and `setY(Object, Object)` bridges.
#[test]
fn a_member_extension_override_of_a_generic_slot_gets_erased_bridges() {
    let lib = "interface G<T> {\n\
               \x20   val T.x: String\n\
               \x20   var T.y: T\n\
               }\n";
    let src = "class Impl : G<String> {\n\
               \x20   override val String.x: String get() = this\n\
               \x20   override var String.y: String\n\
               \x20       get() = this\n\
               \x20       set(value) {}\n\
               }\n";
    for bridge in [
        "public java.lang.String getX(java.lang.Object)",
        "public java.lang.Object getY(java.lang.Object)",
        "public void setY(java.lang.Object, java.lang.Object)",
    ] {
        assert_same_method("MemberExtensionOverrideBridge", lib, src, "Impl", bridge);
    }
}

/// Over a value-class receiver the accessor is mangled, its guard names the receiver as kotlinc's
/// replacement function does (`$this$x`), and the generic bridge unboxes into it.
#[test]
fn a_value_class_receiver_override_bridges_into_the_mangled_accessor() {
    let lib = "@JvmInline value class S(val v: String)\n\
               interface G<T> { val T.x: String }\n";
    let src = "object Impl : G<S> { override val S.x: String get() = v }\n";
    for method in [
        "public java.lang.String getX(java.lang.Object)",
        "public java.lang.String getX-",
    ] {
        assert_same_method("ValueClassReceiverOverride", lib, src, "Impl", method);
    }
}

/// Delegated member-extension properties, generic and not, over a value-class receiver. Each read
/// and write links only if the forwarders and bridges exist under kotlinc's names, and a read
/// through the generic interface boxes the receiver it passes. The accessors are reached from
/// plain extension helpers whose dispatch receiver is the delegating class, so no stdlib inline
/// function is involved; member helpers would themselves be delegated and bypass the forwarders.
#[test]
fn delegated_member_extension_properties_forward_every_accessor() {
    let src = "@JvmInline value class S(val v: String)\n\
               interface I {\n\
               \x20   val S.x: String\n\
               \x20   var S.y: String\n\
               }\n\
               interface G<T> {\n\
               \x20   val T.x: String\n\
               \x20   var T.y: String\n\
               }\n\
               object IImpl : I {\n\
               \x20   override val S.x: String get() = v\n\
               \x20   override var S.y: String\n\
               \x20       get() = v + \"!\"\n\
               \x20       set(value) { last = v + value }\n\
               \x20   var last = \"\"\n\
               }\n\
               object GImpl : G<S> {\n\
               \x20   override val S.x: String get() = v\n\
               \x20   override var S.y: String\n\
               \x20       get() = v + \"?\"\n\
               \x20       set(value) { last = v + value }\n\
               \x20   var last = \"\"\n\
               }\n\
               class D : I by IImpl\n\
               class E : G<S> by GImpl\n\
               fun I.readX(s: S): String = s.x\n\
               fun I.readY(s: S): String = s.y\n\
               fun I.writeY(s: S, value: String) { s.y = value }\n\
               fun <T> G<T>.readX(t: T): String = t.x\n\
               fun <T> G<T>.readY(t: T): String = t.y\n\
               fun <T> G<T>.writeY(t: T, value: String) { t.y = value }\n\
               fun <T> generic(g: G<T>, t: T): String {\n\
               \x20   g.writeY(t, \"w\")\n\
               \x20   return g.readX(t) + g.readY(t)\n\
               }\n\
               fun box(): String {\n\
               \x20   val d = D()\n\
               \x20   d.writeY(S(\"a\"), \"b\")\n\
               \x20   if (d.readX(S(\"c\")) + d.readY(S(\"d\")) != \"cd!\") return \"I\"\n\
               \x20   if (IImpl.last != \"ab\") return \"I set\"\n\
               \x20   val e = E()\n\
               \x20   if (e.readX(S(\"e\")) + e.readY(S(\"f\")) != \"ef?\") return \"G\"\n\
               \x20   if (generic(E(), S(\"g\")) != \"gg?\") return \"generic\"\n\
               \x20   if (GImpl.last != \"gw\") return \"G set\"\n\
               \x20   return \"OK\"\n\
               }\n";
    assert_box_ok("DelegatedMemberExtensionProperties", src);
}
