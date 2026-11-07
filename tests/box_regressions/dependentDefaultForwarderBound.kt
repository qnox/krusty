interface Source<E> {
fun <T : E, U : T> echo(value: U): U = value
}
class Applied : Source<String>
fun box(): String {
val result = Applied().echo<String, String>("OK")
return if (result == "OK") "OK" else "fail: $result"
}
