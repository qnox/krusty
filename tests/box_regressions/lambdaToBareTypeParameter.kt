// A lambda passed where a bare type parameter is expected keeps its natural function type, and that
// type infers the parameter: `id { 42 }` is `() -> Int`, so its result can be invoked. The lambda
// still returns to the call's label, including inside a builder whose type variable it infers.
class Holder { fun <T> id(value: T): T = value }
fun <T> top(value: T): T = value
class Buildee<T> { var last: T? = null; fun yield(value: T) { last = value } }
fun <T> build(instructions: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>().apply(instructions)
fun box(): String {
    val f = Holder().id { 42 }
    val g = top { "O" }
    val h = Holder().id { x: Int -> x + 1 }
    val labeled = Holder().id { return@id 7 }
    val built = build { yield { return@yield "K" } }
    return if (f() == 42 && h(1) == 2 && labeled() == 7) g() + built.last!!() else "fail"
}
