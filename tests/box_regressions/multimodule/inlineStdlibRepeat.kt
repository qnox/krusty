// MODULE: lib
fun dependencyMarker() = Unit
// MODULE: main(lib)
fun box(): String {
    var count = 0
    repeat(2) { count++ }
    return if (count == 2) "OK" else "fail"
}
