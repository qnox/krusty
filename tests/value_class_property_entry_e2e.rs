//! A value class implements an interface property through ordinary instance entries on its box.
//!
//! kotlinc realizes each accessor of a value class's computed property as a static over the
//! carrier (`getB-impl(carrier)`, `setB-impl(carrier, value)`), exactly like a member function, and
//! gives the box one public instance entry per accessor that overrides an interface accessor,
//! declared right after its static: the getter, the setter of a `var` implementing a `var`, and the
//! member-extension getter taking its receiver (`getC(String)`). Each entry answers to the
//! accessor's own JVM name, mangled when its signature mentions a value class (`getT-<hash>`), and
//! anchors on the accessor's own line. A setter added by a `var` implementing a `val` overrides
//! nothing and has no entry.

use super::common;

const SOURCE: &str = "@JvmInline value class Tag(val n: Int)\n\
                      var sink = 0\n\
                      interface Shape {\n\
                      \x20   val a: Int\n\
                      \x20   var b: Int\n\
                      \x20   val String.c: String\n\
                      \x20   val t: Tag\n\
                      \x20   var t2: Tag\n\
                      \x20   val String.e: Tag\n\
                      \x20   val r: Int\n\
                      }\n\
                      @JvmInline value class Box(val x: Int) : Shape {\n\
                      \x20   override val a: Int get() = x\n\
                      \x20   override var b: Int\n\
                      \x20       get() = x + 1\n\
                      \x20       set(value) { sink = value + x }\n\
                      \x20   override val String.c: String get() = \"$this$x\"\n\
                      \x20   override val t: Tag get() = Tag(7)\n\
                      \x20   override var t2: Tag\n\
                      \x20       get() = Tag(8)\n\
                      \x20       set(v) { sink = x + 100 }\n\
                      \x20   override val String.e: Tag get() = Tag(x)\n\
                      \x20   override var r: Int\n\
                      \x20       get() = x\n\
                      \x20       set(value) { sink = value }\n\
                      }\n\
                      fun Shape.readC(text: String): String = text.c\n\
                      fun Shape.readE(text: String): Tag = text.e\n\
                      fun box(): String {\n\
                      \x20   val s: Shape = Box(1)\n\
                      \x20   if (s.a != 1) return \"a\"\n\
                      \x20   if (s.b != 2) return \"b\"\n\
                      \x20   s.b = 5\n\
                      \x20   if (sink != 6) return \"setB\"\n\
                      \x20   if (s.readC(\"k\") != \"k1\") return \"c\"\n\
                      \x20   if (s.t.n != 7) return \"t\"\n\
                      \x20   if (s.t2.n != 8) return \"t2\"\n\
                      \x20   s.t2 = Tag(9)\n\
                      \x20   if (sink != 101) return \"setT2\"\n\
                      \x20   if (s.readE(\"e\").n != 1) return \"e\"\n\
                      \x20   if (s.r != 1) return \"r\"\n\
                      \x20   return \"OK\"\n\
                      }\n";

/// The box declares kotlinc's members in kotlinc's order, each with kotlinc's flags, code and
/// debug tables: every accessor's entry right after its static, none for `r`'s setter.
#[test]
fn a_value_class_declares_kotlincs_property_entries() {
    let comparison =
        common::assert_class_code_matches_kotlinc("ValueClassPropertyEntry", SOURCE, "Box");
    // The public instance accessors: the plain getter of the primary property, then one entry per
    // overriding accessor (the members the comparison above already placed after their statics).
    let accessors = common::member_table(&comparison.krusty_bytes)
        .into_iter()
        .filter_map(|member| {
            let signature = member.strip_prefix("method 0x0001 ")?;
            let (name_and_descriptor, _) = signature.split_once(' ')?;
            Some(name_and_descriptor.to_string())
        })
        .filter(|member| member.starts_with("get") || member.starts_with("set"))
        .collect::<Vec<_>>();
    assert_eq!(
        accessors,
        [
            "getA()I",
            "getB()I",
            "setB(I)V",
            "getC(Ljava/lang/String;)Ljava/lang/String;",
            "getT-s4Pxd3k()I",
            "getT2-s4Pxd3k()I",
            "setT2-txdesME(I)V",
            "getE-OHOa8zQ(Ljava/lang/String;)I",
            "getR()I",
        ]
    );
}

#[test]
fn value_class_property_entries_answer_interface_calls() {
    let output = common::compile_and_run_box(
        SOURCE,
        "ValueClassPropertyEntryBox",
        &[common::stdlib_jar()],
        None,
    )
    .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
