//! A lambda assigned to a CLASSPATH property whose declared type is a (receiver) function type
//! (`var handler: (Scope.(Req) -> Resp)? = null`) never received an expected-type shape: the
//! property's type came from the erased JVM descriptor (raw `FunctionN`, all-`Any`), so the
//! lambda body's bare member/extension calls on the receiver and its parameter types were
//! unresolved ("unresolved reference 'tag'"), while kotlinc is clean. The property type is now
//! recovered through the shared metadata type projection used by properties and member returns.
//! The projection verifies that the logical type and physical accessor have the same JVM erasure,
//! then publishes one type to the property/getter/setter while leaving the invocation descriptor
//! opaque. The lambda therefore binds its receiver and parameter without an accessor-name or
//! function-property special case. Verified end-to-end against a kotlinc-compiled dependency.
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
/// property type selected by the shared same-erasure projection.
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

/// A SUSPEND fun-typed classpath property (`var onEvent: (suspend (Req) -> Resp)? = null`).
/// Metadata spells the type as `Function2<Req, Continuation<Resp>, Any?>` plus a suspend TYPE flag;
/// a lambda literal assigned to it is a 1-parameter suspend lambda to kotlinc. Checked, not run:
/// driving the suspend call needs a coroutine runtime not on this classpath — the fact under test
/// is that the assignment resolves cleanly (no arity/type mismatch, `req.tag` resolves).
#[test]
fn a_lambda_assigned_to_a_classpath_suspend_fun_typed_property_checks_clean() {
    const SUSPEND_LIB: &str = "package lib\n\
         class Req(val tag: String)\n\
         class Resp(val body: String)\n\
         class Config { var onEvent: (suspend (Req) -> Resp)? = null }\n";
    let main = "import lib.Config\n\
        import lib.Resp\n\
        fun f(c: Config) {\n\
        \x20 c.onEvent = { req -> Resp(req.tag) }\n\
        }\n";
    if let Some(diagnostics) =
        common::checker_diags_against("cpfuntypedpropsuspend", SUSPEND_LIB, main)
    {
        assert!(
            diagnostics.is_empty(),
            "a suspend fun-typed classpath property assignment must check clean, got: {diagnostics:#?}"
        );
    }
}

/// A classpath MEMBER (not a property accessor) returning a function type: the recovered return
/// comes from the raw JVM `Signature` (`Function1<List<Integer>, List<Integer>>`), which spells
/// collections and boxed primitives in Java form. The recovery must canonicalize them
/// (`java/util/List<java/lang/Integer>` → `kotlin/collections/List<kotlin/Int>`) exactly as the
/// parameterized-`Obj` return recovery does — else members on the invoked result (`.sum()`) are
/// unresolved and the argument check compares Kotlin against Java spellings.
#[test]
fn a_classpath_member_fun_typed_return_canonicalizes_collections() {
    const MAKER_LIB: &str = "package lib\n\
         class Maker {\n\
         \x20 fun make(): (List<Int>) -> List<Int> = { xs -> xs.map { it + 1 } }\n\
         }\n";
    let main = "import lib.Maker\n\
        fun box(): String {\n\
        \x20 val f = Maker().make()\n\
        \x20 val n = f(listOf(1, 2)).sum()\n\
        \x20 if (n != 5) return \"fail: \" + n\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpfunretcanon", MAKER_LIB, main);
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
