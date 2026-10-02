// Kotlin's answers for `map_lookup_protocol.c`: every call a lookup, a `put`, a removal and
// `containsValue` make into the keys and values, and what each answers. `K` and `A` are the program
// classes `logged_keys.h` stands in for: `A`'s `equals` accepts a `K` that never accepts it back.
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

class A(val n: Int, val tag: String) {
    override fun equals(other: Any?): Boolean {
        log.append("eqA($tag) ")
        return other is K && other.n == n
    }

    override fun hashCode(): Int {
        log.append("hashA($tag) ")
        return 7
    }

    override fun toString() = tag
}

fun box(): String = buildString {
    fun show(label: String, v: Any?) {
        appendLine("$label = $v | $log")
        log.setLength(0)
    }
    val a1 = K(1, 7, "a1")
    val b = K(2, 7, "b")
    val c = K(3, 8, "c")
    val m = LinkedHashMap<Any, Int>()
    show("put a1", m.put(a1, 1))
    show("put b", m.put(b, 2))
    show("put c", m.put(c, 3))
    show("get a1", m[a1])
    show("get q", m[K(1, 7, "q")])
    show("get r", m[K(9, 7, "r")])
    show("get s", m[K(9, 99, "s")])
    show("containsKey c", m.containsKey(c))
    show("get x", m[A(1, "x")])
    show("remove b", m.remove(b))
    show("get c", m[c])
    val s = LinkedHashSet<Any>()
    show("add a1", s.add(a1))
    show("add c", s.add(c))
    show("contains z", s.contains(A(1, "z")))
    show("add d", s.add(K(1, 7, "d")))
    show("remove e", s.remove(K(3, 8, "e")))
    val vm = LinkedHashMap<String, Any>()
    show("put x", vm.put("x", a1))
    show("put y", vm.put("y", c))
    show("containsValue f", vm.containsValue(K(3, 0, "f")))
    show("containsValue a1", vm.containsValue(a1))
    show("containsValue g", vm.containsValue(A(3, "g")))
    val f = LinkedHashMap<Any, Int>()
    show("fresh get a", f[K(1, 7, "a")])
    show("fresh containsKey b", f.containsKey(K(1, 7, "b")))
    show("fresh getOrDefault o", f.getOrDefault(K(1, 7, "o"), 5))
    show("fresh remove c", f.remove(K(1, 7, "c")))
    val fs = LinkedHashSet<Any>()
    show("fresh set contains f", fs.contains(K(1, 7, "f")))
    show("fresh set remove g", fs.remove(K(1, 7, "g")))
    show("put j", f.put(K(1, 7, "j"), 1))
    f.clear()
    show("cleared get k", f[K(1, 7, "k")])
}
