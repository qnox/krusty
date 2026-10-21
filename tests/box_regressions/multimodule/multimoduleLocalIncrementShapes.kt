// MODULE: lib
fun exerciseUpdates(): String {
    var local = 1
    val oldLocal = local++
    ++local
    if (oldLocal != 1 || local != 3) return "local"
    return "OK"
}
// MODULE: main(lib)
fun box() = exerciseUpdates()
