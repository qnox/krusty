// LANGUAGE: +MultiPlatformProjects
// MODULE: common
// FILE: Common.kt
expect fun expected(): String
interface Contract { fun member(): String }
abstract class Base : Contract { abstract override fun member(): String }
fun commonValue(): String = expected()
// MODULE: main()()(common)
// FILE: Platform.kt
actual fun expected(): String = "OK"
class Value : Base() { override fun member(): String = commonValue() }
fun box(): String = Value().member()
