// The `kotlin.concurrent.atomics` classes and their members. On the JVM kotlinc maps them onto
// `java.util.concurrent.atomic` (`AtomicInt` is `AtomicInteger`, `load` is `get`, `fetchAndAdd` is
// `getAndAdd`, `size` is `length`), while the stdlib's extensions on them stay ordinary calls. A
// generic atomic infers its type argument from the constructor argument.
@file:OptIn(kotlin.concurrent.atomics.ExperimentalAtomicApi::class)

import kotlin.concurrent.atomics.*

fun ints(): String {
    val a = AtomicInt(1)
    a.store(5)
    if (a.load() != 5) return "load ${a.load()}"
    if (a.fetchAndAdd(2) != 5) return "fetchAndAdd"
    if (a.addAndFetch(3) != 10) return "addAndFetch"
    if (!a.compareAndSet(10, 11) || a.compareAndSet(10, 12)) return "compareAndSet"
    if (a.exchange(4) != 11) return "exchange"
    a += 2
    a -= 1
    if (a.incrementAndFetch() != 6 || a.fetchAndDecrement() != 6) return "increments"
    if (a.toString() != "5") return "toString $a"
    return "OK"
}

fun longs(): String {
    val a = AtomicLong(1L)
    a.store(5L)
    if (a.fetchAndAdd(2L) != 5L || a.addAndFetch(3L) != 10L) return "add"
    if (!a.compareAndSet(10L, 11L) || a.exchange(4L) != 11L) return "exchange"
    if (a.fetchAndIncrement() != 4L || a.load() != 5L) return "load ${a.load()}"
    return "OK"
}

fun booleans(): String {
    val a = AtomicBoolean(false)
    a.store(true)
    if (!a.load() || !a.exchange(false) || !a.compareAndSet(false, true)) return "boolean"
    return "OK"
}

fun references(): String {
    val a = AtomicReference("x")
    a.store("y")
    if (a.exchange("z") != "y" || !a.compareAndSet("z", "w")) return "reference"
    val load: String = a.load()
    return if (load == "w") "OK" else "load $load"
}

fun arrays(): String {
    val ints = AtomicIntArray(3)
    ints.storeAt(1, 7)
    if (ints.fetchAndAddAt(1, 2) != 7 || ints.addAndFetchAt(1, 1) != 10) return "int add"
    if (ints.exchangeAt(1, 3) != 10 || !ints.compareAndSetAt(1, 3, 4)) return "int exchange"
    if (ints.loadAt(1) != 4 || ints.size != 3) return "int load"

    val longs = AtomicLongArray(2)
    longs.storeAt(0, 7L)
    if (longs.fetchAndAddAt(0, 2L) != 7L || longs.addAndFetchAt(0, 1L) != 10L) return "long add"
    if (longs.exchangeAt(0, 3L) != 10L || !longs.compareAndSetAt(0, 3L, 4L)) return "long exchange"
    if (longs.loadAt(0) != 4L) return "long load"

    val references = AtomicArray(arrayOf("a", "b"))
    references.storeAt(1, "c")
    if (references.exchangeAt(1, "d") != "c" || !references.compareAndSetAt(1, "d", "e")) {
        return "reference exchange"
    }
    val load: String = references.loadAt(1)
    return if (load == "e" && references.size == 2) "OK" else "reference load $load"
}

fun box(): String {
    ints().let { if (it != "OK") return it }
    longs().let { if (it != "OK") return it }
    booleans().let { if (it != "OK") return it }
    references().let { if (it != "OK") return it }
    return arrays()
}
