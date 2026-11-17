fun present(choice: String): String? = if (choice == "no") null else choice

var seen = ""

fun edit(installed: String?, choice: String) {
val row = if (installed == null) present(choice) ?: return else installed
seen = seen + row + ";"
}

interface Marker

fun panel(block: () -> Marker): Marker = block()

class Host {
fun create(): Marker {
return panel {
object : Marker {}
}
}
}

fun box(): String {
edit("kept", "x")
edit(null, "yes")
edit(null, "no")
if (seen != "kept;yes;") return seen
return "OK"
}
