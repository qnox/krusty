package demo
import kotlinx.serialization.Serializable
@Serializable
class Foo(val a: Int, val b: String = "x")
