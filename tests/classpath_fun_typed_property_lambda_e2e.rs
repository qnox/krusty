//! A lambda assigned to a CLASSPATH property whose declared type is a (receiver) function type
//! (`var handler: (Scope.(Req) -> Resp)? = null`) never received an expected-type shape: the
//! property's type came from the erased JVM descriptor (raw `FunctionN`, all-`Any`), so the
//! lambda body's bare member/extension calls on the receiver and its parameter types were
//! unresolved ("unresolved reference 'tag'"), while kotlinc is clean. The property type is now
//! recovered from the accessor's generic `Signature`/`@Metadata` shape — the same publish a
//! member value parameter gets — so the lambda binds its receiver and parameter. Verified
//! end-to-end on a real JVM against a kotlinc-compiled dependency.
use super::common;

const LIB: &str = "package lib\n\
     class Scope { fun status(): String = \"S\" }\n\
     class Req(val tag: String)\n\
     class Resp(val body: String)\n\
     class Config {\n\
     \x20 var handler: (Scope.(Req) -> Resp)? = null\n\
     \x20 fun run(tag: String): String {\n\
     \x20   val h = handler ?: return \"none\"\n\
     \x20   return Scope().h(Req(tag)).body\n\
     \x20 }\n\
     }\n";

#[test]
fn a_lambda_assigned_to_a_classpath_receiver_fun_typed_property_binds_shape() {
    let main = "import lib.Config\n\
        import lib.Resp\n\
        fun box(): String {\n\
        \x20 val c = Config()\n\
        \x20 c.handler = { req -> Resp(status() + \":\" + req.tag) }\n\
        \x20 val r = c.run(\"T\")\n\
        \x20 if (r != \"S:T\") return \"fail: \" + r\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpfuntypedprophandler", LIB, main);
}

/// The READ side of the same property: `val h = c.handler ?: return` must carry the full shape —
/// the nullable wrapper (else the elvis is meaningless) and the RECEIVER mark, so the local can be
/// invoked receiver-style (`Scope().h(Req("T"))`). The getter's JVM `Signature` recovers the
/// parameter/return classes but cannot spell the receiver mark; that comes from the `@Metadata`
/// property type overlaid on the accessor.
#[test]
fn a_read_classpath_receiver_fun_typed_property_invokes_receiver_style() {
    let main = "import lib.Config\n\
        import lib.Req\n\
        import lib.Resp\n\
        import lib.Scope\n\
        fun box(): String {\n\
        \x20 val c = Config()\n\
        \x20 c.handler = { req -> Resp(status() + \":\" + req.tag) }\n\
        \x20 val h = c.handler ?: return \"no handler\"\n\
        \x20 val r = Scope().h(Req(\"T\")).body\n\
        \x20 if (r != \"S:T\") return \"fail: \" + r\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpfuntypedpropread", LIB, main);
}

#[test]
fn a_lambda_assigned_to_a_classpath_plain_fun_typed_property_infers_params() {
    const PLAIN_LIB: &str = "package lib\n\
         class Box {\n\
         \x20 var op: ((Int, Int) -> Int)? = null\n\
         \x20 fun apply(a: Int, b: Int): Int = op?.invoke(a, b) ?: -1\n\
         }\n";
    let main = "import lib.Box\n\
        fun box(): String {\n\
        \x20 val b = Box()\n\
        \x20 b.op = { x, y -> x + y }\n\
        \x20 if (b.apply(2, 3) != 5) return \"fail: \" + b.apply(2, 3)\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpfuntypedpropplain", PLAIN_LIB, main);
}
