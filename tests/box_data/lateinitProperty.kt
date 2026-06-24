// WITH_STDLIB
// Krusty-curated: `lateinit var` — a backing field with no initializer plus synthesized accessors.
// The getter null-checks the field and throws UninitializedPropertyAccessException when unset (that
// guard is emitted and JVM-verified here); once assigned, the getter returns the value.

class Holder {
    lateinit var value: String
}

fun box(): String {
    val h = Holder()
    h.value = "OK"
    return h.value
}
