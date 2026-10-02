// Kotlin's answers for `comparable_range_members.c`: `ComparableRange`'s `equals`, `hashCode` and
// `toString` over a program's own `Comparable`, each line the answer (or the exception it stopped
// at) and the exact calls it made into the program. `V` is the program class the driver builds a
// stand-in for; `throws` names its members that throw: 'c' compareTo, 'e' equals, 'h' hashCode,
// 's' toString.
val log = StringBuilder()

class Boom(tag: String) : RuntimeException(tag)

class V(val n: Int, val tag: String, val throws: String = "") : Comparable<V> {
    override fun compareTo(other: V): Int {
        log.append("cmp($tag,${other.tag}) ")
        if ('c' in throws) throw Boom("cmp:$tag")
        return n.compareTo(other.n)
    }

    override fun equals(other: Any?): Boolean {
        log.append("eq($tag) ")
        if ('e' in throws) throw Boom("eq:$tag")
        return other is V && other.n == n
    }

    override fun hashCode(): Int {
        log.append("hash($tag) ")
        if ('h' in throws) throw Boom("hash:$tag")
        return n
    }

    override fun toString(): String {
        log.append("str($tag) ")
        if ('s' in throws) throw Boom("str:$tag")
        return "V$n"
    }
}

fun box(): String = buildString {
    fun show(label: String, block: () -> Any?) {
        val answer = try { block().toString() } catch (b: Boom) { "threw ${b.message}" }
        appendLine("$label = $answer | $log")
        log.setLength(0)
    }
    val a = V(1, "a")..V(3, "b")
    val same = V(1, "c")..V(3, "d")
    val empty1 = V(5, "e")..V(2, "f")
    val empty2 = V(9, "g")..V(0, "h")
    val other = V(1, "i")..V(4, "j")
    show("a==same") { a == same }
    show("a==empty1") { a == empty1 }
    show("empty1==empty2") { empty1 == empty2 }
    show("empty1==a") { empty1 == a }
    show("a==other") { a == other }
    show("a.hash") { a.hashCode() }
    show("empty1.hash") { empty1.hashCode() }
    show("a.str") { a.toString() }
    show("empty1.str") { empty1.toString() }
    show("cmpThrows==a") { (V(1, "p", "c")..V(3, "q")) == a }
    show("empty1==emptyOtherThrows") { empty1 == (V(1, "r", "c")..V(0, "s")) }
    show("eqThrows==same") { (V(1, "t", "e")..V(3, "u")) == same }
    show("endEqThrows==same") { (V(1, "v")..V(3, "w", "e")) == same }
    show("endHashThrows.hash") { (V(1, "k")..V(3, "l", "h")).hashCode() }
    show("startHashThrows.hash") { (V(1, "m", "h")..V(3, "n", "h")).hashCode() }
    show("startStrThrows.str") { (V(1, "o", "s")..V(3, "y", "s")).toString() }
    show("endStrThrows.str") { (V(1, "z")..V(3, "zz", "s")).toString() }
}
