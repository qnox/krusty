// MODULE: lib
package fixture
class MutableCell<T>(initial: T) {
    private var stored: T = initial
    var writes: Int = 0
    var value: T
        get() = stored
        set(value) { stored = value; writes++ }
}
fun stringCell(): MutableCell<String> = MutableCell("before")
fun intCell(): MutableCell<Int> = MutableCell(0)
// MODULE: main(lib)
fun box(): String {
    val string = fixture.stringCell()
    val number = fixture.intCell()
    string.value = "OK"
    number.value = 42
    if (string.value != "OK" || string.writes != 1) return "String setter"
    if (number.value != 42 || number.writes != 1) return "Int setter"
    return "OK"
}
