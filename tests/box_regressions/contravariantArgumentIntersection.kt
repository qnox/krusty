// Unrelated contravariant arguments meet at their intersection, as kotlinc's common supertype
// intersects them: `Sink<A>` and `Sink<B>` join at `Sink<A & B>`, which accepts an `AB` that is
// both. An empty meet at `Nothing` would reject the call.
interface Sink<in T> { fun put(t: T): String }
interface A { fun a(): String = "O" }
interface B { fun b(): String = "K" }
class AB : A, B
class SinkA : Sink<A> { override fun put(t: A): String = t.a() }
class SinkB : Sink<B> { override fun put(t: B): String = t.b() }

fun <T> pick(first: T, second: T, takeFirst: Boolean): T = if (takeFirst) first else second

fun box(): String {
    val first = pick(SinkA(), SinkB(), takeFirst = true)
    val second = pick(SinkA(), SinkB(), takeFirst = false)
    return first.put(AB()) + second.put(AB())
}
