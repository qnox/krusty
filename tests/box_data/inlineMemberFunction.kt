// WITH_STDLIB
// Krusty-curated: a SIMPLE inline member function (no reified type params, no inline lambda params) is
// emitted as an ordinary method and called as a normal invokevirtual — inlining is only an
// optimization here, so the behaviour is identical to a regular member call.

class Greeter {
    inline fun greet(): String = build()
    @PublishedApi internal fun build(): String = "OK"
}

fun box(): String = Greeter().greet()
