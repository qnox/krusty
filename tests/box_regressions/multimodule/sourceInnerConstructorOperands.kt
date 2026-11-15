// MODULE: lib
package fixture
class Envelope(val prefix: String) {
    inner class Part(val suffix: String) { fun result(): String = prefix + suffix }
}
// MODULE: main(lib)
fun box(): String = fixture.Envelope("O").Part("K").result()
