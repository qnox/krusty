// Kotlin's answers for `map_equality_across_kinds.c`: map, set, view and entry equality and hash
// codes across the four classes, with every call each makes into the keys and values, an exception
// thrown inside `equals`, and a change to the map inside a comparison. `K` is the program class
// `logged_keys.h` stands in for; `Throws` and `Acting` are the driver's.
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

class Throws(val tag: String, val kind: Int) {
    override fun equals(other: Any?): Boolean {
        log.append("throw($tag) ")
        when (kind) {
            0 -> throw NullPointerException()
            1 -> throw ClassCastException()
            else -> throw IllegalStateException()
        }
    }

    override fun hashCode(): Int {
        log.append("hash($tag) ")
        return 7
    }
}

var act: (() -> Unit)? = null

class Acting(val n: Int) {
    override fun equals(other: Any?): Boolean {
        val x = act
        act = null
        x?.invoke()
        return other is Acting && other.n == n
    }

    override fun hashCode() = 7
}

fun box(): String = buildString {
    fun show(label: String, f: () -> Any?) {
        val answer = try {
            f().toString()
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label = $answer | $log")
        log.setLength(0)
    }
    val a = K(1, 7, "a")
    val b = K(2, 7, "b")
    val va = K(10, 1, "va")
    val vb = K(20, 2, "vb")
    val m1 = LinkedHashMap<Any, Any?>()
    m1[a] = va
    m1[b] = vb
    val a2 = K(1, 7, "a2")
    val b2 = K(2, 7, "b2")
    val va2 = K(10, 1, "va2")
    val vb2 = K(20, 2, "vb2")
    val m2 = HashMap<Any, Any?>()
    m2[b2] = vb2
    m2[a2] = va2
    log.setLength(0)
    show("m1 == m2") { m1 == m2 }
    show("m2 == m1") { m2 == m1 }
    show("m1 == m1") { m1 == m1 }
    show("m1.hashCode()") { m1.hashCode() }
    val n1 = LinkedHashMap<Any, Any?>()
    n1[a] = null
    val n2 = HashMap<Any, Any?>()
    n2[a2] = null
    val n3 = HashMap<Any, Any?>()
    n3[b2] = null
    log.setLength(0)
    show("n1 == n2") { n1 == n2 }
    show("n1 == n3") { n1 == n3 }
    val s1 = LinkedHashSet<Any>()
    s1.add(a)
    s1.add(b)
    val s2 = HashSet<Any>()
    s2.add(b2)
    s2.add(a2)
    log.setLength(0)
    show("s1 == s2") { s1 == s2 }
    show("s2 == s1") { s2 == s1 }
    show("s1 == s1") { s1 == s1 }
    show("s1.hashCode()") { s1.hashCode() }
    show("s1 == m1.keys") { s1 == m1.keys }
    show("m1.keys == s1") { m1.keys == s1 }
    show("m1.entries == m2.entries") { m1.entries == m2.entries }
    show("m1.entries.hashCode()") { m1.entries.hashCode() }
    val e1 = m1.entries.first()
    val e2 = HashMap<Any, Any?>().apply { put(a2, va2) }.entries.first()
    log.setLength(0)
    show("e1 == e2") { e1 == e2 }
    show("e1.hashCode()") { e1.hashCode() }
    show("e1 == Pair(a, va)") { (e1 as Any) == Pair(a, va) }
    show("m1 == s1") { (m1 as Any) == s1 }
    show("s1 == listOf(a, b)") { (s1 as Any) == listOf(a, b) }
    val pm = LinkedHashMap<Any, Any?>()
    pm[Throws("x", 0)] = "1"
    val pm2 = LinkedHashMap<Any, Any?>()
    pm2[Throws("y", 0)] = "1"
    log.setLength(0)
    show("pm == pm2") { pm == pm2 }
    val cm = LinkedHashMap<Any, Any?>()
    cm["k"] = Throws("v", 1)
    val cm2 = LinkedHashMap<Any, Any?>()
    cm2["k"] = "5"
    log.setLength(0)
    show("cm == cm2") { cm == cm2 }
    val ps = LinkedHashSet<Any>()
    ps.add(Throws("s", 0))
    val ps2 = LinkedHashSet<Any>()
    ps2.add(Throws("t", 0))
    log.setLength(0)
    show("ps == ps2") { ps == ps2 }
    val im = LinkedHashMap<Any, Any?>()
    im["k"] = Throws("i", 2)
    log.setLength(0)
    show("im == cm2") { im == cm2 }
    val c1 = LinkedHashMap<Any, Any?>()
    c1["k"] = Acting(1)
    c1["l"] = Acting(2)
    val c2 = LinkedHashMap<Any, Any?>()
    c2["k"] = Acting(1)
    c2["l"] = Acting(2)
    act = { c1["z"] = "1" }
    show("c1 == c2") { c1 == c2 }
}
