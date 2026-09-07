//! A bare name inside a lambda with receiver: a lexical local or parameter of an enclosing scope
//! wins over the receiver's member of the same name, even though the receiver lambda is nested
//! inside the local's scope (`val headers = authHeaders(); client.get(url) { headers.forEach … }`
//! must read the local map, not `HttpRequestBuilder.headers`). Only a receiver-derived binding —
//! an outer class's own property — yields to an inner implicit receiver's member. Measured against
//! kotlinc 2.4.10: both shapes compile, and the box run pins which binding each reads.

use super::common;

#[test]
fn a_local_beats_an_inner_receiver_member_but_an_outer_property_does_not() {
    const SRC: &str = "class Builder {\n\
    val headers: MutableList<Int> = mutableListOf(1, 2, 3)\n\
    val x: Int = 7\n\
}\n\
fun build(block: Builder.() -> Unit) { Builder().block() }\n\
fun local(): Int {\n\
    val headers = mapOf(\"a\" to \"b\", \"c\" to \"d\")\n\
    var n = 0\n\
    build { headers.forEach { (k, v) -> n += k.length + v.length } }\n\
    return n\n\
}\n\
fun parameter(headers: Map<String, String>): Int {\n\
    var n = 0\n\
    build { n = headers.keys.size }\n\
    return n\n\
}\n\
class Outer(val x: Int) {\n\
    fun read(): Int { var r = 0; build { r = x }; return r }\n\
}\n\
fun captured(): Int {\n\
    val x = 9\n\
    class Local { fun read(): Int { var r = 0; build { r = x }; return r } }\n\
    return Local().read()\n\
}\n\
fun box(): String {\n\
    val l = local()\n\
    val p = parameter(mapOf(\"k\" to \"v\"))\n\
    val o = Outer(1).read()\n\
    val c = captured()\n\
    return if (l == 4 && p == 1 && o == 7 && c == 9) \"OK\" else \"FAIL: $l $p $o $c\"\n\
}\n";
    let diagnostics = common::front_end_diagnostics_files_with_stdlib(&[SRC]);
    assert_eq!(diagnostics, Vec::<String>::new());
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("shadowing shapes compile + run"),
        "OK"
    );
}
