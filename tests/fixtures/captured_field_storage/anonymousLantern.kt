class Lantern(val glow: String) {
fun light(): String {
val wick = object {
inner class Flame { fun shine() = glow }
fun burn() = Flame().shine()
}
return wick.burn()
}
}
fun box(): String = Lantern("OK").light()
