interface Marker

class Del(val value: String) {
operator fun getValue(thisRef: Any?, property: Any?): String = value
}

class Receiver(val value: String)

operator fun Receiver.getValue(thisRef: Any?, property: Any?): String = value

fun panel(block: () -> Marker): Marker = block()

class Host {
val member by Del("member")
val extension by Receiver("extension")
fun create(): Marker {
return panel {
object : Marker {}
}
}
}

fun box(): String {
val host = Host()
if (host.member != "member") return "member"
if (host.extension != "extension") return "extension"
return "OK"
}
