// MODULE: lib
import kotlin.experimental.*
fun inlineBehavior(): String {
    val a: Byte = 0x0F
    val b: Byte = 0x3C
    if ((a and b) != 0x0C.toByte()) return "fail: and"
    if ((a or b) != 0x3F.toByte()) return "fail: or"
    if (a.inv() != (-16).toByte()) return "fail: inv"
    return "OK"
}
// MODULE: main(lib)
fun box(): String = inlineBehavior()
