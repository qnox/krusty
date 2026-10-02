// Kotlin's answers for `map_kinds_and_views.c`: the classes a map is made of, its views and its
// entries, what each view shows and does, and how an iterator fails once the map has changed. The
// values are strings, as the driver's are. `K` is the program class `logged_keys.h` stands in for.
val log = StringBuilder()

class K(val n: Int, val h: Int, val tag: String) {
    override fun equals(other: Any?): Boolean {
        log.append("eq($tag,${(other as? K)?.tag}) ")
        return other is K && other.n == n
    }

    override fun hashCode(): Int {
        log.append("hash($tag) ")
        return h
    }

    override fun toString() = tag
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
    fun names(vararg xs: Any) =
        xs.joinToString(" ") { "${it::class.qualifiedName} ${it::class.simpleName}" }

    val m = LinkedHashMap<String, Any>()
    m["a"] = "1"
    m["b"] = "2"
    val ks = m.keys
    val vs = m.values
    val es = m.entries
    appendLine("${ks === m.keys} ${vs === m.values} ${es === m.entries}")
    m["c"] = "3"
    appendLine("$ks $vs $es ${ks.size} ${vs.size} ${es.size}")
    val e = es.first()
    m["a"] = "10"
    appendLine("$e ${e.key} ${e.value}")
    appendLine(names(e, ks, vs, es))
    val h = HashMap<String, Any>()
    h["x"] = "1"
    appendLine(names(h.entries.first(), h.keys, h.values, h.entries))
    val va: Any = vs
    val ka: Any = ks
    val ea: Any = es
    val xa: Any = e
    appendLine("${va is List<*>} ${va == listOf("10", "2", "3")} ${vs == vs} " +
        "${va is Collection<*>} ${ka is Set<*>} ${ea is Set<*>} ${xa is Map.Entry<*, *>} " +
        "${xa is MutableMap.MutableEntry<*, *>}")
    val ma: Any = m
    val ha: Any = h
    val ls: Any = LinkedHashSet<Any>()
    val hs: Any = HashSet<Any>()
    appendLine("${ma is HashMap<*, *>} ${ha is LinkedHashMap<*, *>} ${ls is HashSet<*>} " +
        "${hs is LinkedHashSet<*>} ${ma is MutableMap<*, *>} ${hs is MutableSet<*>}")
    appendLine(names(ma, ha, hs, ls))
    t("keys.remove(b)") { ks.remove("b") }
    appendLine(m)
    t("values.remove(3)") { vs.remove("3") }
    appendLine(m)
    t("keys.add(z)") { ks.add("z") }
    t("values.add(4)") { vs.add("4") }
    appendLine(m)
    val it = m.iterator()
    m["q"] = "5"
    t("next after a put") { it.next() }
    val it2 = m.keys.iterator()
    while (it2.hasNext()) it2.next()
    m["r"] = "6"
    t("exhausted next after a put") { it2.next() }
    val it3 = m.values.iterator()
    while (it3.hasNext()) it3.next()
    t("exhausted next") { it3.next() }
    val it4 = m.keys.iterator()
    m["a"] = "99"
    t("next after an overwrite") { it4.next() }
    val it5 = m.keys.iterator()
    m.clear()
    appendLine("hasNext after a clear: ${it5.hasNext()}")
    t("next after a clear") { it5.next() }
    appendLine("$ks ${ks.size}")
    m["k"] = "1"
    appendLine("$ks $vs $es")
    val other = HashMap<String, Any>()
    other["k"] = "1"
    appendLine("${es.contains(other.entries.first())} ${es.remove(other.entries.first())} $m")
    val loop = LinkedHashMap<String, Any>()
    loop["x"] = "1"
    loop["y"] = "2"
    appendLine(buildString { for ((k, v) in loop) append("$k$v ") })
    val keyed = LinkedHashSet<Any>()
    keyed.add(K(1, 7, "a"))
    keyed.add(K(2, 8, "b"))
    appendLine("added | $log")
    log.setLength(0)
    val iterable: Iterable<Any> = keyed
    appendLine("in: ${K(2, 8, "q") in iterable} | $log")
}
