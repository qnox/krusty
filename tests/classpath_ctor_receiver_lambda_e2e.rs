//! A CLASSPATH constructor whose parameter is a RECEIVER function type (`Base(init: Cfg.() -> Unit)`).
//! Both the JVM descriptor and the `Signature` attribute erase `Cfg.() -> Unit` to `Function1`, and a
//! constructor is absent from `@Metadata`'s FUNCTION records — so the receiver mark that member/top-level
//! callables recover was never restored for one. The lambda argument then bound no implicit `this` and a
//! bare member call inside it was "unresolved reference". Round-tripped on a real JVM.
use super::common;

const LIB: &str = "package lib\n\
     class Cfg { var v: Int = 0; fun set(x: Int) { v = x } }\n\
     open class Base(val init: Cfg.() -> Unit)\n";

#[test]
fn a_lambda_passed_to_a_classpath_receiver_lambda_constructor_binds_this() {
    let main = "import lib.Base\n\
        import lib.Cfg\n\
        fun box(): String {\n\
        \x20 val c = Cfg()\n\
        \x20 Base({ set(42) }).init(c)\n\
        \x20 return if (c.v == 42) \"OK\" else \"v=${c.v}\"\n\
        }\n";
    common::expect_box_ok_against_ref("cp_ctor_recv_lambda", LIB, main);
}

/// The same shape through a SUPER-constructor delegation in a class header — the other route that
/// reads the constructor's parameter shapes.
#[test]
fn a_class_header_super_call_binds_the_receiver_lambda_this() {
    let main = "import lib.Base\n\
        import lib.Cfg\n\
        class A : Base({ set(7) })\n\
        fun box(): String {\n\
        \x20 val c = Cfg()\n\
        \x20 A().init(c)\n\
        \x20 return if (c.v == 7) \"OK\" else \"v=${c.v}\"\n\
        }\n";
    common::expect_box_ok_against_ref("cp_super_ctor_recv_lambda", LIB, main);
}

/// A class with SEVERAL constructors of the same arity, only one of which takes a receiver lambda.
/// `@Metadata` constructor records align to `<init>` by arity alone, so the receiver marks cannot be
/// attributed to one of them — applying the first record's marks to both rewrote the ordinary
/// `(Int) -> Unit` parameter into a receiver function type and made this call unresolvable.
const AMBIGUOUS_LIB: &str = "package lib\n\
     class Cfg { var v: Int = 0; fun set(x: Int) { v = x } }\n\
     class Dual {\n\
     \x20 constructor(a: Int, f: Cfg.() -> Unit)\n\
     \x20 constructor(a: String, g: (Int) -> Unit)\n\
     }\n";

#[test]
fn a_same_arity_constructor_set_keeps_its_plain_function_parameter() {
    let main = "import lib.Dual\n\
        fun box(): String {\n\
        \x20 var seen = 0\n\
        \x20 Dual(\"s\") { n -> seen = n }\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cp_ctor_same_arity", AMBIGUOUS_LIB, main);
}
