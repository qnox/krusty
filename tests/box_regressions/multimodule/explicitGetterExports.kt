// MODULE: lib
package fixture
class Computed<T>(private val stored: T) {
    var reads: Int = 0
    val value: T get() { reads++; return stored }
}
fun stringCell(value: String): Computed<String> = Computed(value)
fun intCell(value: Int): Computed<Int> = Computed(value)
// MODULE: main(lib)
fun box(): String {
    val string = fixture.stringCell("OK")
    val number = fixture.intCell(42)
    if (string.value != "OK" || string.reads != 1) return "String getter"
    if (number.value != 42 || number.reads != 1) return "Int getter"
    return "OK"
}
