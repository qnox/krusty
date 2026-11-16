// MODULE: common
// A common caller leaves the arguments of an expected constructor to the defaults the `expect`
// declares; the platform's actual constructor receives them.
expect class Box(value: String = "O", suffix: String = value + "K") {
    val result: String
}
fun commonBox(): String = Box().result
// MODULE: main()()(common)
// FILE: call.kt
fun box(): String {
    if (commonBox() != "OOK") return "Fail common: " + commonBox()
    if (Box("A").result != "AAK") return "Fail platform: " + Box("A").result
    if (Box("A", "B").result != "AB") return "Fail explicit"
    return "OK"
}
// FILE: actual.kt
actual class Box actual constructor(value: String, suffix: String) {
    actual val result: String = value + suffix
}
