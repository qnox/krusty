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
    common::expect_box_ok_against("cp_ctor_recv_lambda", LIB, main);
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
    common::expect_box_ok_against("cp_super_ctor_recv_lambda", LIB, main);
}
