// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: the JVM's ArrayList.addAll copies its argument before the list grows, so a
// range of the list itself is read whole. The Native and Wasm stdlib's ArrayList grows first, and
// the range then fails its modification check: kotlinc-wasm throws ConcurrentModificationException.
fun ints(vararg values: Int): ArrayList<Int> {
    val list = ArrayList<Int>()
    for (value in values) list.add(value)
    return list
}

fun box(): String {
    val root = ints(1, 2, 3)
    val view = (root as List<Int>).subList(0, 2)
    root.addAll(view)
    if (root != listOf(1, 2, 3, 1, 2)) return "aliased $root"
    val appended = ints(1, 2, 3)
    val head = (appended as List<Int>).subList(0, 2)
    appended.addAll(head)
    if (appended != ints(1, 2, 3, 1, 2)) return "aliased addAll"
    return "OK"
}
