// Kotlin's answers for `map_callback_mutation.c`: a lookup, a `put`, `toString` and
// `containsValue` whose key's `equals` or `toString` changes the map. `K` is the program class the
// driver stands in for: its next `equals` or `toString` runs `action` once.
var action: (() -> Unit)? = null

fun act() {
    val a = action
    action = null
    a?.invoke()
}

class K(val n: Int, val tag: String, val h: Int = 7) {
    override fun equals(other: Any?): Boolean {
        act()
        return other is K && other.n == n
    }

    override fun hashCode(): Int = h

    override fun toString(): String {
        act()
        return tag
    }
}

fun box(): String = buildString {
    fun t(label: String, f: () -> Any?) {
        val answer = try {
            f().toString()
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label: $answer")
    }
    val a = K(1, "a")
    val b = K(2, "b")
    val m = LinkedHashMap<Any, Any?>()
    m[a] = "A"
    m[b] = "B"
    action = { m.remove(a) }
    t("LinkedHashMap get removing its match") { m[K(1, "q")] }
    t("then") { m }
    val h = HashMap<Any, Any?>()
    h[a] = "A"
    h[b] = "B"
    action = { h.remove(a) }
    t("HashMap get removing another key") { h[K(2, "q")] }
    t("then") { h }
    val p = LinkedHashMap<Any, Any?>()
    p[a] = "A"
    action = { p[K(5, "e")] = "E" }
    t("put adding a key") { p.put(K(9, "n"), "N") }
    t("then") { p }
    val s = LinkedHashMap<Any, Any?>()
    s[a] = "A"
    s["x"] = "X"
    action = { s["y"] = "Y" }
    t("toString adding a key") { s.toString() }
    t("then") { s }
    val cv = LinkedHashMap<Any, Any?>()
    cv["k"] = a
    cv["l"] = b
    action = { cv.remove("k") }
    t("LinkedHashMap containsValue removing its entry") { cv.containsValue(K(2, "w")) }
    t("then") { cv }
    val cv2 = HashMap<Any, Any?>()
    cv2["k"] = a
    cv2["l"] = b
    action = { cv2.remove("k") }
    t("HashMap containsValue removing its entry") { cv2.containsValue(K(2, "w")) }
    t("then") { cv2 }
}
