//! Member (instance) functions with default parameters, realized via the same `$default` mechanism as
//! data-class `copy`: the JVM backend emits `name$default(self, params…, mask, marker)` and a call with
//! omitted args passes a mask. One node — `MethodCall` with `args[i] = None` for an omitted argument.
//! Round-tripped under `-Xverify:all`.

mod common;

#[test]
fn member_default_args_run() {
    const SRC: &str = "class C {\n\
fun add(a: Int, b: Int = 10): Int = a + b\n\
fun greet(name: String, greeting: String = \"Hi\"): String = greeting + \" \" + name\n\
}\n\
fun box(): String {\n\
val c = C()\n\
if (c.add(1) != 11) return \"f1\"\n\
if (c.add(1, 2) != 3) return \"f2\"\n\
if (c.greet(\"X\") != \"Hi X\") return \"f3\"\n\
if (c.greet(\"Y\", greeting = \"Yo\") != \"Yo Y\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "D");
}

/// Default-argument INHERITANCE: an override declared without defaults reuses the base method's
/// defaults (`open class A { open fun foo(x: Int = 42) }`, `class C : B()` where `B : A()`, `C` overrides
/// `foo(x)`; `C().foo()` fills `42` and dispatches to the override). Kotlin forbids re-declaring the
/// default on the override, so the value must come from the base declaration.
#[test]
fn inherited_default_args_run() {
    const SRC: &str = "open class A { open fun foo(x: Int = 42): Int = x }\n\
open class B : A()\n\
class C : B() { override fun foo(x: Int): Int = x + 1 }\n\
abstract class Base { abstract fun bar(s: String = \"abc\"): String }\n\
class Derived : Base() { override fun bar(s: String): String = s }\n\
fun box(): String {\n\
if (C().foo() != 43) return \"f1\"\n\
if (C().foo(10) != 11) return \"f2\"\n\
if (Derived().bar() != \"abc\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "DI");
}
