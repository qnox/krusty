// A class literal's `isInstance` tests a class another file declares, with no ordinary `is` of it
// in the calling file.
// FILE: shapes.kt
open class Shape
class Circle : Shape()
class Square : Shape()

fun circle(): Any = Circle()

// FILE: main.kt
fun box(): String {
    val value = circle()
    if (!Circle::class.isInstance(value)) return "fail: own class"
    if (!Shape::class.isInstance(value)) return "fail: supertype"
    if (Square::class.isInstance(value)) return "fail: sibling"
    if (Circle::class.isInstance(null)) return "fail: null"
    return "OK"
}
