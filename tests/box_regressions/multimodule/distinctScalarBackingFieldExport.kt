// LANGUAGE: +ExplicitBackingFields
// MODULE: lib
package fixture
class Counted {
    val count: Number
        field: Int = 2
    fun twice(): Int = count + count
}
fun counted(): Counted = Counted()
fun localCount(): Number = Counted().count
// MODULE: main(lib)
fun box(): String {
    val counted = fixture.counted()
    if (counted.count != 2) return "exported storage"
    if (fixture.localCount() != 2) return "local storage"
    if (counted.twice() != 4) return "field read"
    return "OK"
}
