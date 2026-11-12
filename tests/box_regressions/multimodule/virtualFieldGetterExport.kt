// A read through the base type runs the override of a field-backed open property.
// MODULE: lib
package fixture

open class Base {
    open val value: String = "base"
}

class Derived : Base() {
    override val value: String = "OK"
}

fun produce(): Base = Derived()

// MODULE: main(lib)
fun box(): String = fixture.produce().value
