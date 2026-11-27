var topLevel = "t"

class Holder(val tag: String) {
    var mirrored: String by ::topLevel
    val own: String by Holder::tag
}

val viaTop: String by ::topLevel

val Holder.viaExtension: String by Holder::tag

object Source {
    val held: Int = 4
}

class Borrower {
    val borrowed by Source::held
}

fun box(): String {
    val holder = Holder("h")
    holder.mirrored = "m"
    val result = topLevel + holder.mirrored + holder.own + viaTop + holder.viaExtension +
        Borrower().borrowed
    return if (result == "mmhmh4") "OK" else result
}
