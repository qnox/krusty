// Kotlin's answers for `map_mutated_source.c`: `map` and `forEach` over a `MutableList` the lambda
// changes. Kotlin's `map` is `mapTo(ArrayList(size))`, a `for` loop over the list's iterator, so
// the size only sizes the result: the walk asks `hasNext()` before every element, the JVM's
// `ArrayList` answers it as `cursor != size`, and `next()` after a structural change throws
// `ConcurrentModificationException`. Each line is the answer and the list afterwards.
fun box(): String = buildString {
    fun t(label: String, source: MutableList<Int>, walk: () -> Any?) {
        val answer = try {
            walk().toString()
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label: $answer $source")
    }
    val one = mutableListOf(1)
    t("map appending to [1]", one) { one.map { one.add(it + 1); it * 10 } }
    val two = mutableListOf(1, 2)
    t("map appending at the last of [1, 2]", two) { two.map { if (it == 2) two.add(3); it * 10 } }
    val three = mutableListOf(1, 2)
    t("map removing the first at the first of [1, 2]", three) {
        three.map { if (it == 1) three.removeAt(0); it * 10 }
    }
    val four = mutableListOf(1, 2)
    t("map removing the last at the last of [1, 2]", four) {
        four.map { if (it == 2) four.removeAt(1); it * 10 }
    }
    val five = mutableListOf(1, 2)
    t("map setting the second at the first of [1, 2]", five) {
        five.map { if (it == 1) five[1] = 5; it * 10 }
    }
    val six = mutableListOf(1, 2)
    t("forEach removing the last at the last of [1, 2]", six) {
        six.forEach { if (it == 2) six.removeAt(1) }
    }
    val seven = mutableListOf(1, 2)
    t("map of an unchanged [1, 2]", seven) { seven.map { it * 10 } }
}
