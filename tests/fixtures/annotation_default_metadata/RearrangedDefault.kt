// PLATFORM-SPECIFIC: pins the JVM Kotlin metadata of an annotation parameter default whose
// nested annotation passes its arguments out of declaration order. The portable KT-73845 box
// program joins tests/box_regressions with the change that makes it pass on Wasm-JS.

annotation class A
annotation class B(
    val a: String = "Fail",
    val b: Array<A>
)

annotation class C(
    // `a` and `b` parameters are misplaced
    val value: B = B(b = [A()], a = "OK")
)

fun box(): String {
    val c = C()
    return c.value.a
}
