// WITH_STDLIB
// A same-arity `get` overload beside the `CharSequence.get` override must not hide the override
// from the for-loop walk, whether the length is a stored property or a getter, and a subclass
// inherits the walk.
open class Stored(val s: String) : CharSequence {
    fun get(key: String): Char = throw IllegalStateException("get(String) must not be called")
    override val length = s.length
    override fun subSequence(startIndex: Int, endIndex: Int) = s.subSequence(startIndex, endIndex)
    override fun get(index: Int) = s[index]
}

class Derived(s: String) : Stored(s)

class Computed(val s: String) : CharSequence {
    fun get(key: Long): Char = throw IllegalStateException("get(Long) must not be called")
    override val length: Int get() = s.length
    override fun subSequence(startIndex: Int, endIndex: Int) = s.subSequence(startIndex, endIndex)
    override fun get(index: Int) = s[index]
}

fun walk(cs: CharSequence): String {
    val result = StringBuilder()
    for (c in cs) result.append(c)
    return result.toString()
}

fun box(): String {
    val stored = StringBuilder()
    for (c in Stored("1234")) stored.append(c)
    if (stored.toString() != "1234") return "stored: $stored"
    val derived = StringBuilder()
    for (c in Derived("abc")) derived.append(c)
    if (derived.toString() != "abc") return "derived: $derived"
    val computed = StringBuilder()
    for (c in Computed("xyz")) computed.append(c)
    if (computed.toString() != "xyz") return "computed: $computed"
    if (walk(Computed("q")) != "q") return "as CharSequence"
    return "OK"
}
