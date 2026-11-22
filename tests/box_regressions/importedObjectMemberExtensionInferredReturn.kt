import Ref.asRef

class Resolved(val value: String)

object Ref {
    fun String.asRef() = Resolved(this)
}

fun use(s: String) = s.asRef()

fun box(): String = use("OK").value
