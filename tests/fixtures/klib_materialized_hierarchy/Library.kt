package fixture

interface Root<T>
interface Leaf<T> : Root<T>
open class Parent<T> : Leaf<T>
class Child : Parent<String>()
class Unrelated

fun make(): Any = Child()
fun other(): Any = Unrelated()
