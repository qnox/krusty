// Two files read members of one library object. A target that emits library bodies realizes the
// object once for the whole program, with every member either file reaches.
// FILE: first.kt
fun firstSize(): Int = emptyList<Int>().size

// FILE: main.kt
fun box(): String {
    val empty = emptyList<Int>()
    if (firstSize() != 0 || !empty.isEmpty() || empty.contains(1)) return "FAIL"
    return "OK"
}
