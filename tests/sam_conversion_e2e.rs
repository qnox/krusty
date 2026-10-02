//! SAM conversion: a lambda passed where a (simple, non-generic) `fun interface` is expected becomes an
//! instance of that interface whose single abstract method runs the lambda. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn lambda_to_fun_interface_argument() {
    const SRC: &str = "fun interface Foo { fun get(): String }\n\
fun call(f: Foo): String = f.get()\n\
fun box(): String = call { \"OK\" }\n";
    assert_eq!(run(SRC).expect("lambda -> fun interface arg"), "OK");
}

#[test]
fn fun_interface_with_param() {
    // The lambda's parameter is typed from the SAM method; the lowered impl matches the SAM descriptor.
    const SRC: &str = "fun interface Transform { fun apply(x: String): String }\n\
fun run2(t: Transform): String = t.apply(\"O\")\n\
fun box(): String = run2 { s -> s + \"K\" }\n";
    assert_eq!(run(SRC).expect("fun interface with param"), "OK");
}

#[test]
fn generic_fun_interface() {
    // A generic SAM erases to Object; the erased descriptor matches.
    const SRC: &str = "fun interface C<T> { fun f(x: T): T }\n\
fun r(c: C<String>): String = c.f(\"O\")\n\
fun box(): String = r { it + \"K\" }\n";
    assert_eq!(run(SRC).expect("generic fun interface"), "OK");
}

#[test]
fn contravariant_sam_consumes_nested_projection_capture() {
    const SRC: &str = r#"
        abstract class Base
        interface Marker
        class Left : Base(), Marker
        class Right : Base(), Marker

        fun interface Consumer<T> { fun accept(value: T) }

        class Holder<T> {
            fun consume(consumer: Consumer<in T>) {
                consumer.accept(object : Base() {} as T)
            }
        }

        fun acceptAny(value: Any) {}

        fun box(): String {
            val holder = if ("".length == 0) Holder<Left>() else Holder<Right>()
            holder.consume {}
            holder.consume(::acceptAny)
            return "OK"
        }
    "#;
    assert_eq!(run(SRC).expect("nested projected SAM conversion"), "OK");
}

#[test]
fn a_fun_interface_subtype_keeps_its_identity() {
    const SRC: &str = "fun interface Foo : () -> Int\n\
        fun id(foo: Foo): Any = foo\n\
        fun box(): String {\n\
            val value = object : Foo {\n\
                override fun invoke(): Int = 42\n\
                override fun toString(): String = \"OK\"\n\
            }\n\
            val passed = id(value)\n\
            if (value !== passed) return \"Fail identity\"\n\
            return passed.toString()\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "FunInterfaceSubtypeIdentity");
}

#[test]
fn a_class_implementing_a_fun_interface_and_a_function_type_is_passed_through() {
    const SRC: &str = "interface Mark\n\
        fun interface Worker { suspend fun onStart(scope: Mark) }\n\
        class Impl(val name: String) : Worker, (String) -> Unit {\n\
            override suspend fun onStart(scope: Mark) {}\n\
            override fun toString(): String = name\n\
            override fun invoke(value: String) {}\n\
        }\n\
        var result = \"\"\n\
        fun foo(worker: Worker) { result += worker.toString() }\n\
        fun bar(vararg worker: Worker) { result += worker[0].toString() }\n\
        fun box(): String {\n\
            foo(Impl(\"O\"))\n\
            bar(Impl(\"K\"))\n\
            return result\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "FunInterfaceAndFunctionSupertype");
}

#[test]
fn a_generic_fun_interface_subtype_keeps_its_to_string() {
    const SRC: &str = "interface Mark\n\
        fun interface Worker<E> { suspend fun onStart(scope: E) }\n\
        class Impl<F>(val name: String) : Worker<F>, (String) -> Unit {\n\
            override suspend fun onStart(scope: F) {}\n\
            override fun toString(): String = name\n\
            override fun invoke(value: String) {}\n\
        }\n\
        var result = \"\"\n\
        fun <T> foo(worker: Worker<T>) { result += worker.toString() }\n\
        fun <T> bar(vararg worker: Worker<T>) { result += worker[0].toString() }\n\
        fun box(): String {\n\
            foo(Impl<Mark>(\"O\"))\n\
            bar(Impl<Mark>(\"K\"))\n\
            return result\n\
        }\n";
    common::expect_box_same_as_kotlinc(SRC, "GenericFunInterfaceSubtype");
}

#[test]
fn actual_interface_instance_still_passes() {
    // A real implementing class passed where the fun interface is expected must NOT be SAM-converted.
    const SRC: &str = "fun interface Foo { fun get(): String }\n\
class A : Foo { override fun get() = \"OK\" }\n\
fun call(f: Foo): String = f.get()\n\
fun box(): String = call(A())\n";
    assert_eq!(run(SRC).expect("interface instance passes through"), "OK");
}
