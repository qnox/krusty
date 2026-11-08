interface Callback { fun get(): String }
interface Result { fun read(): String }
open class Base(val fn: Callback)
class Host(val unused: String) {
fun make(value: String): Result = object : Result {
override fun read(): String {
val middle = object : Base(object : Callback {
override fun get() = value
}) {}
return middle.fn.get()
}
}
}
fun box(): String = Host("unused").make("OK").read()
