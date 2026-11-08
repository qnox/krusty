// WITH_STDLIB
// The standard library's atomics and a @Volatile field, on every target: kotlin.concurrent.atomics
// AtomicInt, AtomicLong, AtomicBoolean and AtomicReference with each of their operations.
@file:OptIn(kotlin.concurrent.atomics.ExperimentalAtomicApi::class)

import kotlin.concurrent.Volatile
import kotlin.concurrent.atomics.AtomicBoolean
import kotlin.concurrent.atomics.AtomicInt
import kotlin.concurrent.atomics.AtomicLong
import kotlin.concurrent.atomics.AtomicReference
import kotlin.concurrent.atomics.decrementAndFetch
import kotlin.concurrent.atomics.fetchAndIncrement
import kotlin.concurrent.atomics.incrementAndFetch

class Flag {
    @Volatile
    var raised = false
}

fun ints(): String {
    val counter = AtomicInt(5)
    val log = StringBuilder()
    log.append(counter.load()).append(' ')
    counter.store(7)
    log.append(counter.fetchAndAdd(3)).append(' ')
    log.append(counter.addAndFetch(2)).append(' ')
    log.append(counter.exchange(1)).append(' ')
    log.append(counter.compareAndSet(1, 4)).append(' ')
    log.append(counter.compareAndSet(1, 9)).append(' ')
    log.append(counter.compareAndExchange(4, 6)).append(' ')
    log.append(counter.incrementAndFetch()).append(' ')
    log.append(counter.fetchAndIncrement()).append(' ')
    log.append(counter.decrementAndFetch()).append(' ')
    log.append(counter.load())
    return log.toString()
}

fun longs(): String {
    val total = AtomicLong(1L shl 40)
    total.addAndFetch(5L)
    val swapped = total.compareAndSet((1L shl 40) + 5L, -2L)
    return "${total.load()} $swapped ${total.exchange(3L)} ${total.fetchAndAdd(-4L)} ${total.load()}"
}

fun booleans(): String {
    val open = AtomicBoolean(false)
    val first = open.compareAndSet(false, true)
    val second = open.compareAndSet(false, true)
    return "$first $second ${open.exchange(false)} ${open.load()}"
}

fun references(): String {
    val head = AtomicReference<String?>(null)
    val claimed = head.compareAndSet(null, "a")
    val missed = head.compareAndSet(null, "b")
    val previous = head.exchange("c")
    head.store(head.load() + "d")
    return "$claimed $missed $previous ${head.load()}"
}

fun box(): String {
    val ints = ints()
    if (ints != "5 7 12 12 true false 4 7 7 7 7") return "ints: $ints"
    val longs = longs()
    if (longs != "-2 true -2 3 -1") return "longs: $longs"
    val booleans = booleans()
    if (booleans != "true false true false") return "booleans: $booleans"
    val references = references()
    if (references != "true false a cd") return "references: $references"
    val flag = Flag()
    flag.raised = true
    if (!flag.raised) return "volatile"
    return "OK"
}
