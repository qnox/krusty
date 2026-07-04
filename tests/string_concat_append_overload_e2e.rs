//! `StringBuilder.append` overload selection for a String value whose type was parsed from a method
//! RETURN descriptor (a classpath call, or the data-class `Arrays.toString(field)` wrapper) — such a
//! value is `Ty::Obj("java/lang/String")`, not `Ty::String`, and krusty appended it via the
//! less-specific `append(Object)` instead of `append(String)` (a per-concat divergence from kotlinc).
//! Now `append_top` treats the String class as `String`. These run on a real JVM to guard the result;
//! byte-parity itself is checked by the differential harness (`bytediff dataClasses/equals/intarray`).

use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let cp = common::classpath_jars_for("// WITH_STDLIB");
    let cs = common::compile_in_process(src, "S", &cp, Some(&jdk))?;
    let box_class = common::find_box_class(&cs)?;
    common::run_box(&cs, &box_class, &cp)
}

#[test]
fn data_class_with_array_field_tostring() {
    // The IntArray field is rendered via `Arrays.toString(a)` (returns String) then appended — the
    // path that exposed `append(Object)`. The result must read back exactly.
    let src = "data class P(val a: IntArray)\n\
        fun box(): String {\n\
        \x20   val s = P(intArrayOf(1, 2)).toString()\n\
        \x20   return if (s == \"P(a=[1, 2])\") \"OK\" else \"fail:\" + s\n\
        }\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: toolchain unavailable"),
    }
}

#[test]
fn concat_with_classpath_string_returning_call() {
    // `uppercase()` is a classpath String method: its return type comes from the call descriptor
    // (`Ty::Obj("java/lang/String")`), the case that previously picked `append(Object)`.
    let src = "fun box(): String {\n\
        \x20   val r = \"x=\" + \"ab\".uppercase()\n\
        \x20   return if (r == \"x=AB\") \"OK\" else \"fail:\" + r\n\
        }\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: toolchain unavailable"),
    }
}

#[test]
fn string_get_uses_interface_owner_dispatch() {
    // `String.get(Int)` can resolve to a classpath member physically owned by `CharSequence`.
    // The emitted call must use interface dispatch for that owner.
    let src = "fun box(): String {\n\
        \x20   val c = \"OK\"[1]\n\
        \x20   return if (c == 'K') \"OK\" else \"fail:\" + c\n\
        }\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: toolchain unavailable"),
    }
}

#[test]
fn string_extensions_resolve_on_implicit_receiver() {
    let src = "fun box(): String {\n\
        \x20   val up = \"ab\".run { uppercase() }\n\
        \x20   if (up != \"AB\") return \"uppercase:\" + up\n\
        \x20   val tr = \"  OK  \".run { trim() }\n\
        \x20   return tr\n\
        }\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: toolchain unavailable"),
    }
}

#[test]
fn string_for_loop_uses_library_members() {
    let src = "fun box(): String {\n\
        \x20   var out = \"\"\n\
        \x20   for (c in \"OK\") out += c\n\
        \x20   return out\n\
        }\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: toolchain unavailable"),
    }
}
