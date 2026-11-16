// MODULE: common
// A common caller leaves the arguments of an expected member to the defaults the `expect`
// declares; the platform's actual member evaluates them.
expect class Renderer() {
    fun render(value: String = "O", suffix: String = value + "K"): String
}
fun commonRender(): String = Renderer().render()
// MODULE: main()()(common)
// FILE: call.kt
fun box(): String {
    if (commonRender() != "OOK") return "Fail common: " + commonRender()
    if (Renderer().render("A") != "AAK") return "Fail platform: " + Renderer().render("A")
    if (Renderer().render("A", "B") != "AB") return "Fail explicit"
    return "OK"
}
// FILE: actual.kt
actual class Renderer actual constructor() {
    actual fun render(value: String, suffix: String): String = value + suffix
}
