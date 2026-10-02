// Kotlin's answers for `empty_map_and_set.c`: what `mapOf` and `setOf` answer for an empty spread,
// and what that answer is and does. `K` is the program class `logged_keys.h` stands in for.
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
    fun names(x: Any) = "${x::class.qualifiedName} ${x::class.simpleName}"

    val m: Map<K, String> = mapOf(*emptyArray<Pair<K, String>>())
    val s: Set<K> = setOf(*emptyArray<K>())
    appendLine("mapOf: ${names(m)}")
    appendLine("setOf: ${names(s)}")
    appendLine("same: ${m === mapOf(*emptyArray<Pair<K, String>>())} " +
        "${s === setOf(*emptyArray<K>())} ${m === emptyMap<K, String>()} ${s === emptySet<K>()}")
    val ma: Any = m
    val sa: Any = s
    appendLine("is: ${ma is Map<*, *>} ${ma is MutableMap<*, *>} ${sa is Set<*>} " +
        "${sa is Collection<*>} ${sa is Iterable<*>} ${sa is MutableSet<*>} " +
        "${sa is MutableCollection<*>} ${sa is MutableIterable<*>}")
    appendLine("as?: ${ma as? MutableMap<*, *>} ${sa as? MutableSet<*>}")
    @Suppress("UNCHECKED_CAST")
    t("put") { (ma as MutableMap<K, String>).put(K(1, 1, "p"), "1") }
    @Suppress("UNCHECKED_CAST")
    t("add") { (sa as MutableSet<K>).add(K(1, 1, "a")) }
    appendLine("render: $m $s ${m.hashCode()} ${s.hashCode()}")
    appendLine("size: ${m.size} ${m.isEmpty()} ${s.size} ${s.isEmpty()}")
    val k = K(1, 1, "k")
    appendLine("lookup: ${m.containsKey(k)} ${m[k]} ${m.getOrDefault(k, "d")} " +
        "${m.containsValue("v")} ${k in s} ${k in (s as Iterable<K>)} | $log")
    appendLine("views: ${m.keys === s} ${m.entries === s} ${m.values} ${m.values.size} " +
        "${m.values.isEmpty()}")
    val i = s.iterator()
    appendLine("iterator: ${names(i)} ${i === s.iterator()} ${i === m.iterator()} ${i.hasNext()}")
    t("next") { i.next() }
    appendLine("walk: ${buildString { for (e in s) append(e); for ((a, b) in m) append("$a$b") }}.")
    val hm = HashMap<K, String>()
    val hs = HashSet<K>()
    appendLine("equals: ${m == hm} ${hm == m} ${s == hs} ${hs == s} ${s == hm.keys} " +
        "${hm.keys == s} ${hm.entries == s} ${m == s} ${s == m} ${s == listOf<K>()}")
    val one = mapOf(k to "1")
    val single = setOf(k)
    log.setLength(0)
    appendLine("unequal: ${m == one} ${one == m} ${s == single} ${single == s} | $log")
    val hm0 = hashMapOf(*emptyArray<Pair<K, String>>())
    val hs0 = hashSetOf(*emptyArray<K>())
    appendLine("hashMapOf: ${names(hm0)} ${hm0 === hashMapOf(*emptyArray<Pair<K, String>>())} " +
        "${(hm0 as Any) is MutableMap<*, *>} ${hm0 == m}")
    appendLine("hashSetOf: ${names(hs0)} ${hs0 === hashSetOf(*emptyArray<K>())} " +
        "${(hs0 as Any) is MutableSet<*>} ${hs0 == s}")
    hm0[k] = "1"
    hs0.add(k)
    appendLine("written: $hm0 $hs0 $m $s")
}
