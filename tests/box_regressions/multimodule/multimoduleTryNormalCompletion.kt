// MODULE: lib
fun normalValue(): Int = try { 42 } catch (value: Throwable) { -1 }
fun normalReturn(): String {
    try { return "OK" } catch (value: Throwable) { return "caught" }
}
// MODULE: main(lib)
fun box(): String {
    if (normalValue() != 42) return "value"
    var count = 0
    while (count < 3) {
        try { count++; continue } catch (value: Throwable) { return "loop" }
    }
    return normalReturn()
}
