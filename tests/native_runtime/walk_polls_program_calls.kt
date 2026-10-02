// Kotlin's answers for `walk_polls_program_calls.c`: every walk over an `Iterable` the program
// declares, over `Seq(listOf(T(0), T(1)), c, 1)` whose `iterator()` ('i'), or whose iterator's
// `hasNext()` ('h') or `next()` ('n') at the second element, throws. Each line is whether the walk
// threw and the calls it made into the program. `T` and `Seq` are the program classes the driver's
// `Tag` and `Seq` (`program_collections.h`) stand in for.
val log = StringBuilder()

class Boom : RuntimeException()

class T(val n: Int) {
    override fun equals(other: Any?): Boolean {
        log.append("eq($n) ")
        return other is T && other.n == n
    }

    override fun hashCode(): Int {
        log.append("hash($n) ")
        return n
    }

    override fun toString(): String {
        log.append("str($n) ")
        return "T$n"
    }
}

class Seq(val xs: List<T>, val throws: Char, val at: Int) : Iterable<T> {
    override fun iterator(): Iterator<T> {
        log.append("iterator ")
        if (throws == 'i') throw Boom()
        return object : Iterator<T> {
            var i = 0
            override fun hasNext(): Boolean {
                log.append("hasNext ")
                if (throws == 'h' && i == at) throw Boom()
                return i < xs.size
            }

            override fun next(): T {
                log.append("next ")
                if (throws == 'n' && i == at) throw Boom()
                return xs[i++]
            }
        }
    }
}

fun f() {
    log.append("f ")
}

fun box(): String = buildString {
    val walks = listOf<Pair<String, (Seq) -> Any?>>(
        "map" to { s -> s.map { f(); it } },
        "forEach" to { s -> s.forEach { f() } },
        "any" to { s -> s.any { f(); false } },
        "all" to { s -> s.all { f(); true } },
        "none" to { s -> s.none { f(); false } },
        "count" to { s -> s.count() },
        "count { }" to { s -> s.count { f(); true } },
        "filter" to { s -> s.filter { f(); true } },
        "firstOrNull" to { s -> s.firstOrNull { f(); false } },
        "first" to { s -> s.first { f(); false } },
        "last" to { s -> s.last { f(); false } },
        "fold" to { s -> s.fold(0) { a, _ -> f(); a } },
        "forEachIndexed" to { s -> s.forEachIndexed { _, _ -> f() } },
        "toList" to { s -> s.toList() },
        "reversed" to { s -> s.reversed() },
        "sortedWith" to { s -> s.sortedWith { _, _ -> f(); 0 } },
        "indexOf" to { s -> s.indexOf(T(9)) },
        "joinToString" to { s -> s.joinToString() },
        "plus" to { s -> s + T(5) },
        "sumOf" to { s -> s.sumOf { f(); 1 } },
        "withIndex" to { s -> s.withIndex().toList() },
        "addAll" to { s -> mutableListOf<T>().addAll(s) },
    )
    val elements = listOf(T(0), T(1))
    for ((name, walk) in walks) {
        for (c in "ihn") {
            val s = Seq(elements, c, 1)
            log.setLength(0)
            val outcome = try {
                walk(s)
                "returned"
            } catch (e: Boom) {
                "threw"
            }
            appendLine("$name $c $outcome | $log")
        }
    }
    // `none()` and `any()` ask `hasNext` once, before the throwing one.
    for (c in "hn") {
        val s = Seq(elements, c, 1)
        log.setLength(0)
        appendLine("none() $c ${s.none()} | $log")
        log.setLength(0)
        appendLine("any() $c ${s.any()} | $log")
    }
}
