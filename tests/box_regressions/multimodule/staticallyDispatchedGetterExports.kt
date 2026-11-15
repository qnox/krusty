// MODULE: lib
package fixture
open class FinalProperty { val value: String = "OK" }
class Inherited : FinalProperty()
open class VirtualProperty { open val value: String = "base" }
class FinalOverride : VirtualProperty() { override val value: String = "OK" }
fun inherited(): FinalProperty = Inherited()
fun overridden(): FinalOverride = FinalOverride()
// MODULE: main(lib)
fun box(): String {
    if (fixture.inherited().value != "OK") return "final property"
    if (fixture.overridden().value != "OK") return "final owner"
    return "OK"
}
