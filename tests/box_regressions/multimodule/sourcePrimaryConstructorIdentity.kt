// MODULE: lib
package fixture
class Choice(val value: String) { constructor(number: Int) : this("secondary") }
// MODULE: main(lib)
fun box(): String = fixture.Choice("OK").value
