// LANGUAGE: +ExplicitBackingFields
// MODULE: lib
package fixture
class Backed {
    val value: Any?
        field: String = "OK"
}
fun backed(): Backed = Backed()
// MODULE: main(lib)
fun box(): String {
    if (fixture.backed().value != "OK") return "backing storage"
    return "OK"
}
