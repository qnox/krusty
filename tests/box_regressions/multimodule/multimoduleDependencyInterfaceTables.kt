// MODULE: lib
interface Shape {
    fun sides(): Int
    fun name(): String = "shape"
}

class Triangle : Shape {
    override fun sides(): Int = 3
}

class Square : Shape {
    override fun sides(): Int = 4
    override fun name(): String = "square"
}

fun describe(shape: Shape): String = shape.name() + shape.sides()

fun describeLibShapes(): String = describe(Triangle()) + " " + describe(Square())

// MODULE: main(lib)
class Hexagon : Shape {
    override fun sides(): Int = 6
}

open class Polygon(private val count: Int) : Shape {
    override fun sides(): Int = count
    override fun name(): String = "polygon"
}

class Octagon : Polygon(8)

fun box(): String {
    val described = describeLibShapes() + " " + describe(Hexagon()) + " " +
        describe(Polygon(5)) + " " + describe(Octagon())
    return if (described == "shape3 square4 shape6 polygon5 polygon8") "OK" else "fail: $described"
}
