//! A lambda in a class member whose body uses a MEMBER EXTENSION of the enclosing class through an
//! EXPLICIT receiver (`ms.map { it.toResponse() }` where `toResponse` is a class-body extension)
//! must capture the enclosing `this` as the extension's dispatch receiver. The capture scan only
//! recognized enclosing-`this` uses spelled as bare names (`this`, or an implicit-`this` member
//! access), so the dispatch use hidden behind `it.` was invisible: `cur_class` was cleared for the
//! closure body, `member_extension_dispatch_value` found no dispatch value, and the file bailed
//! ("this construct is not yet supported by the IR backend"). Member extension FUNCTION calls,
//! PROPERTY reads, and PROPERTY writes all funnel through that dispatch lookup and all failed the
//! same way; the bare-name form (build.840 kk1) and the direct (non-lambda) call already worked.
use super::common;

#[test]
fn member_extension_fun_called_in_lambda() {
    const SRC: &str = "class Model(val id: String)\n\
        data class Resp(val id: String)\n\
        class Ctl {\n\
        \x20 fun list(ms: List<Model>): List<Resp> = ms.map { it.toResponse() }\n\
        \x20 private fun Model.toResponse() = Resp(id = id)\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = Ctl().list(listOf(Model(\"a\"), Model(\"b\")))\n\
        \x20 return if (r.map { it.id } == listOf(\"a\", \"b\")) \"OK\" else \"F:\" + r\n\
        }\n";
    let out = common::expect_box_run_with_stdlib(SRC, "MemberExtFunInLambda");
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "member ext fun via lambda param"
    );
}

#[test]
fn member_extension_property_read_in_lambda() {
    const SRC: &str = "class Model(val id: String)\n\
        class Ctl {\n\
        \x20 fun tags(ms: List<Model>): List<String> = ms.map { it.tag }\n\
        \x20 private val Model.tag: String\n\
        \x20\x20 get() = \"t:\" + id\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = Ctl().tags(listOf(Model(\"x\")))\n\
        \x20 return if (r == listOf(\"t:x\")) \"OK\" else \"F:\" + r\n\
        }\n";
    let out = common::expect_box_run_with_stdlib(SRC, "MemberExtPropReadInLambda");
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "member ext property read in lambda"
    );
}

#[test]
fn member_extension_property_write_in_lambda() {
    const SRC: &str = "class Model(val id: String) { var slot: String = \"\" }\n\
        class Ctl {\n\
        \x20 fun stamp(ms: List<Model>) { ms.map { it.mark = \"m:\" + it.id } }\n\
        \x20 private var Model.mark: String\n\
        \x20\x20 get() = slot\n\
        \x20\x20 set(v) { slot = v }\n\
        }\n\
        fun box(): String {\n\
        \x20 val ms = listOf(Model(\"y\"))\n\
        \x20 Ctl().stamp(ms)\n\
        \x20 return if (ms[0].slot == \"m:y\") \"OK\" else \"F:\" + ms[0].slot\n\
        }\n";
    let out = common::expect_box_run_with_stdlib(SRC, "MemberExtPropWriteInLambda");
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "member ext property write in lambda"
    );
}

#[test]
fn inherited_member_extension_called_in_lambda() {
    // The extension is declared on a BASE class; the lambda sits in the DERIVED class, so the
    // capture decision must accept an owner the enclosing `this` is merely ASSIGNABLE to.
    const SRC: &str = "class Model(val id: String)\n\
        open class Base {\n\
        \x20 protected fun Model.render() = \"r:\" + id\n\
        }\n\
        class Ctl : Base() {\n\
        \x20 fun list(ms: List<Model>): List<String> = ms.map { it.render() }\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = Ctl().list(listOf(Model(\"z\")))\n\
        \x20 return if (r == listOf(\"r:z\")) \"OK\" else \"F:\" + r\n\
        }\n";
    let out = common::expect_box_run_with_stdlib(SRC, "InheritedMemberExtInLambda");
    assert_eq!(out.as_deref(), Some("OK"), "inherited member ext in lambda");
}

#[test]
fn member_extension_in_non_inline_closure() {
    // A NON-inline higher-order function forces a real invokedynamic closure (no splice), so the
    // captured `this` travels as a closure field rather than a remapped slot.
    const SRC: &str = "class Model(val id: String)\n\
        fun runIt(m: Model, f: (Model) -> String): String = f(m)\n\
        class Ctl {\n\
        \x20 fun one(m: Model): String = runIt(m) { it.render() }\n\
        \x20 private fun Model.render() = \"n:\" + id\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = Ctl().one(Model(\"q\"))\n\
        \x20 return if (r == \"n:q\") \"OK\" else \"F:\" + r\n\
        }\n";
    let out = common::expect_box_run_with_stdlib(SRC, "MemberExtNonInlineClosure");
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "member ext in non-inline closure"
    );
}
