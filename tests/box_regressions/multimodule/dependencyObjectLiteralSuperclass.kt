// WITH_STDLIB
// MODULE: lib
// FILE: Library.kt
package fixture

open class Base(val x: Int) {
    open fun describe(): String = "base$x"
    open fun bump(): Int = 0
}

open class Pair2(val a: Int, val b: Int) {
    open fun sum(): Int = a + b
}

inline fun makeBase(k: Int): Base = object : Base(k + 1) {
    var y = k * 2
    override fun describe(): String = "anon${x}_$y"
    override fun bump(): Int {
        y += 1
        return y
    }
}

inline fun makePair(k: Int): Pair2 = object : Pair2(k, k + 10) {
    override fun sum(): Int = super.sum() * 2
}
// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val base = makeBase(3)
    if (base.x != 4) return "superclass argument ${base.x}"
    if (base.describe() != "anon4_6") return "stored property ${base.describe()}"
    if (base.bump() != 7 || base.bump() != 8) return "mutable property"
    if (base.describe() != "anon4_8") return "mutated property ${base.describe()}"
    val other = makeBase(0)
    if (other.x != 1 || other.describe() != "anon1_0") return "second construction ${other.describe()}"
    val pair = makePair(2)
    if (pair.a != 2 || pair.b != 12) return "two superclass arguments ${pair.a} ${pair.b}"
    if (pair.sum() != 28) return "super call ${pair.sum()}"
    return "OK"
}
