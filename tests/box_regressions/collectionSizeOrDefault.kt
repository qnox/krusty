// MODULE: lib
var sizeReads = 0
var iteratorReads = 0

open class SizedValues(val failSize: Boolean) : Collection<Int> {
    override val size: Int
        get() {
            sizeReads++
            if (failSize) throw IllegalStateException("size")
            return 2
        }
    override fun isEmpty(): Boolean = false
    override fun contains(element: Int): Boolean = element == 1 || element == 2
    override fun containsAll(elements: Collection<Int>): Boolean = false
    override fun iterator(): Iterator<Int> {
        iteratorReads++
        return listOf(1, 2).iterator()
    }
}

class InheritedSize : SizedValues(false) {
    override fun iterator(): Iterator<Int> {
        iteratorReads++
        return listOf(3, 4).iterator()
    }
}

class FieldSize : SizedValues(false) {
    override val size: Int = 2
}

class PlainValues : Iterable<Int> {
    override fun iterator(): Iterator<Int> {
        iteratorReads++
        return listOf(5, 6).iterator()
    }
}

fun makeSized(fail: Boolean): Iterable<Int> = SizedValues(fail)
fun makeInherited(): Iterable<Int> = InheritedSize()
fun makeFieldSize(): Iterable<Int> = FieldSize()
fun makePlain(): Iterable<Int> = PlainValues()
fun sizesRead(): Int = sizeReads
fun iteratorsRead(): Int = iteratorReads

// MODULE: main(lib)
fun box(): String {
    val sized = makeSized(false)
    if (sized.map { it + 1 } != listOf(2, 3)) return "sized values"
    if (sizesRead() != 1 || iteratorsRead() != 1) return "sized effects"

    val inherited = makeInherited()
    if (inherited.map { it + 1 } != listOf(4, 5)) return "inherited values"
    if (sizesRead() != 2 || iteratorsRead() != 2) return "inherited effects"

    val fieldSize = makeFieldSize()
    if (fieldSize.map { it + 1 } != listOf(2, 3)) return "field values"
    if (sizesRead() != 2 || iteratorsRead() != 3) return "field effects"

    val plain = makePlain()
    if (plain.map { it + 1 } != listOf(6, 7)) return "plain values"
    if (sizesRead() != 2 || iteratorsRead() != 4) return "plain must not be sized by iteration"

    val throwing = makeSized(true)
    try {
        throwing.map { it + 1 }
        return "missing size exception"
    } catch (failure: IllegalStateException) {
        if (failure.message != "size") return "size exception"
    }
    if (sizesRead() != 3 || iteratorsRead() != 4) return "throwing effects"
    return "OK"
}
