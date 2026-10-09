//! Interface delegation to an EXPRESSION (`class D : I by Impl()`), not just a `val` constructor
//! parameter. The expression is evaluated once and stored in a synthesized `$$delegate` field; each of
//! `I`'s methods forwards to it. Round-tripped on the JVM.

use super::common;

#[test]
fn delegate_to_constructor_call_expression() {
    const SRC: &str = "// WITH_STDLIB\n\
interface Greeter { fun greet(): String }\n\
class Impl : Greeter { override fun greet() = \"OK\" }\n\
class D : Greeter by Impl()\n\
fun box(): String = D().greet()\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn delegate_expression_referencing_constructor_param() {
    // The delegate expression is evaluated in the constructor, so it may reference a ctor parameter.
    const SRC: &str = "// WITH_STDLIB\n\
interface Greeter { fun greet(): String }\n\
class Impl(val s: String) : Greeter { override fun greet() = s }\n\
fun mk(s: String): Greeter = Impl(s)\n\
class D(val x: String) : Greeter by mk(x)\n\
fun box(): String = D(\"OK\").greet()\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn delegate_expression_with_class_body() {
    // The `{ … }` after `by Impl()` is the CLASS BODY, not a trailing lambda on the delegate call.
    const SRC: &str = "// WITH_STDLIB\n\
interface Greeter { fun greet(): String }\n\
class Impl : Greeter { override fun greet() = \"O\" }\n\
class D : Greeter by Impl() {\n\
    fun extra() = \"K\"\n\
}\n\
fun box(): String = D().greet() + D().extra()\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn delegate_expression_on_the_line_after_by() {
    // A newline after `by` continues the clause; the delegate expression starts on the next line.
    const SRC: &str = "// WITH_STDLIB\n\
interface Greeter { fun greet(): String }\n\
interface Named { fun name(): String }\n\
class Impl(val s: String, val t: String = \"\") : Greeter { override fun greet() = s + t }\n\
class D(\n\
    val x: String,\n\
) : Named,\n\
    Greeter by\n\
    Impl(x, t = \"K\") {\n\
    override fun name() = \"\"\n\
}\n\
fun box(): String = D(\"O\").greet() + D(\"\").name()\n";
    common::expect_box_same_as_kotlinc(SRC, "DelegateAfterBy");
}
