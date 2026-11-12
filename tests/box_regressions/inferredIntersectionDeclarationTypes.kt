// A declaration whose type is inferred as an intersection publishes the denotable type kotlinc
// approximates it to: the common supertype of `In<A>` and `In<B>` is `In<A & B>`.
interface In<in E>
class A : In<A>
class B : In<B>

fun <T> select(x: T, y: T) = x ?: y

fun pick(a: A, b: B) = select(a, b)

val picked = select(A(), B())

fun box(): String {
    if (pick(A(), B()) !is A) return "pick"
    if (picked !is A) return "picked"
    return "OK"
}
