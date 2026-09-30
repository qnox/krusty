//! A `vararg` parameter erases to an ARRAY on the JVM, so `of(e: Element)` and
//! `of(vararg a: Element)` have different descriptors — `(LElement;)` and `([LElement;)` — and are
//! legal overloads.
//!
//! The clash key keyed a parameter by its declared type alone, which made every same-name pair of
//! "one element" and "vararg of that element" collide. Those are different Kotlin signatures.
//! `Array<Element>` and `vararg Element` still meet as one JVM descriptor and are a platform
//! declaration clash.
//! Found on intellij-community's `fleet.fastutil` `IntList`/`IntOpenHashSet`, whose companions
//! declare `of()`, `of(e)`, `of(e0, e1)`, `of(e0, e1, e2)` and `of(vararg a)` side by side — the
//! JetBrains Kotlin language server reports nothing there.

use super::common;

#[test]
fn companion_vararg_and_element_overloads_coexist() {
    let src = "class Element\n\
class Payload\n\
class ElementList {\n\
\x20   companion object {\n\
\x20       fun of(): Payload = Payload()\n\
\x20       fun of(e: Element): Payload = Payload()\n\
\x20       fun of(e0: Element, e1: Element): Payload = Payload()\n\
\x20       fun of(vararg a: Element): Payload = Payload()\n\
\x20   }\n\
}\n\
fun box(): String {\n\
\x20   val e = Element()\n\
\x20   ElementList.of()\n\
\x20   ElementList.of(e)\n\
\x20   ElementList.of(e, e)\n\
\x20   ElementList.of(e, e, e)\n\
\x20   return \"OK\"\n\
}\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("VarargCompanionOverloadReference", src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected the companion overload fixture: {reference_stderr}"
    );
    common::expect_box_ok_with_stdlib(src, "VMOC");
}

#[test]
fn member_vararg_and_element_overloads_coexist() {
    let src = "class Element\n\
class Payload\n\
class Sink {\n\
\x20   fun take(value: Element): Payload = Payload()\n\
\x20   fun take(vararg values: Element): Payload = Payload()\n\
}\n\
fun box(): String {\n\
\x20   val s = Sink()\n\
\x20   val e = Element()\n\
\x20   s.take(e)\n\
\x20   s.take(e, e)\n\
\x20   return \"OK\"\n\
}\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("VarargMemberOverloadReference", src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected the member overload fixture: {reference_stderr}"
    );
    common::expect_box_ok_with_stdlib(src, "VMOM");
}

#[test]
fn member_array_and_vararg_of_the_same_element_still_clash() {
    // `Array<Element>` and `vararg Element` are different Kotlin signatures and one JVM descriptor.
    let src = "class Element\n\
class Payload\n\
class Sink {\n\
\x20   fun take(values: Array<Element>): Payload = Payload()\n\
\x20   fun take(vararg values: Element): Payload = Payload()\n\
}\n\
fun box(): String {\n\
\x20   Sink().take(Element())\n\
\x20   return \"OK\"\n\
}\n";
    common::assert_errors_match_kotlinc(&[("Main.kt", src)], &[]);
}
