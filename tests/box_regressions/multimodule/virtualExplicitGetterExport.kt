// A read through the base type runs the override of an explicit open getter.
// MODULE: lib
package fixture

open class Base {
    open val value: String
        get() = "base"
}

class Derived : Base() {
    override val value: String
        get() = "OK"
}

fun produce(): Base = Derived()

// MODULE: main(lib)
fun box(): String = fixture.produce().value
