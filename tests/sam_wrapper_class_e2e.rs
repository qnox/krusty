//! A function value converted to a Kotlin fun interface is wrapped in the class kotlinc generates
//! once per file and interface, `<Facade>$sam$<interface FQ name>$0`, which implements the
//! interface and `FunctionAdapter`. krusty converted it with an `invokedynamic` over a private
//! forwarding method, which failed verification when the method returned a value class.

use super::common;

const SRC: &str = "package p\n\
    @JvmInline value class A(val value: String)\n\
    fun interface B { fun f(x: A): A }\n\
    fun interface I { fun run(x: Int): String }\n\
    fun interface G<T> { fun get(x: T): T }\n\
    fun interface U { fun run() }\n\
    class Outer { fun interface In { fun h(): Int } }\n\
    fun b(f: (A) -> A): B = B(f)\n\
    fun i(f: (Int) -> String): I = I(f)\n\
    class C { fun i(f: (Int) -> String): I = I(f) }\n\
    fun g(f: (String) -> String): G<String> = G(f)\n\
    fun u(f: () -> Unit): U = U(f)\n\
    fun o(f: () -> Int): Outer.In = Outer.In(f)\n";

#[test]
fn each_wrapper_class_matches_kotlinc() {
    for class in [
        "p/SamWrapperKt$sam$p_B$0",
        "p/SamWrapperKt$sam$p_I$0",
        "p/SamWrapperKt$sam$p_G$0",
        "p/SamWrapperKt$sam$p_U$0",
        "p/SamWrapperKt$sam$p_Outer_In$0",
    ] {
        assert_eq!(
            common::byte_diff_against_kotlinc_cp("SamWrapper", SRC, class, &[common::stdlib_jar()])
                .expect("reference kotlinc is provisioned"),
            Ok(()),
            "{class}"
        );
    }
}

#[test]
fn wrappers_of_one_function_are_equal_and_forward_to_it() {
    let src = format!(
        "{SRC}fun box(): String {{\n\
             val f: (A) -> A = {{ A(it.value + \"K\") }}\n\
             val first = b(f)\n\
             val second = b(f)\n\
             if (first != second || first.hashCode() != second.hashCode()) return \"fail\"\n\
             if (C().i {{ \"x\" }}.run(1) != \"x\") return \"fail member\"\n\
             return first.f(A(\"O\")).value\n\
         }}\n"
    );
    assert_eq!(common::expect_box_run_with_stdlib(&src, "Main"), "OK");
}
