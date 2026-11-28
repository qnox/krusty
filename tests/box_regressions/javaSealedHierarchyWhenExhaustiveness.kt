// TARGET_BACKEND: JVM
// FULL_JDK
// PLATFORM-SPECIFIC: Java sealed types state their direct subclasses in the class file's
// `PermittedSubclasses` attribute, which only JVM class files carry. A Java enum whose constants
// have bodies is sealed too, and its constant-body classes are not subclasses a `when` names.
// FILE: Shape.java
public sealed interface Shape permits Circle, Polygon, Kind {}

// FILE: Circle.java
public final class Circle implements Shape {}

// FILE: Polygon.java
public abstract sealed class Polygon implements Shape permits Square, Triangle {}

// FILE: Square.java
public final class Square extends Polygon {}

// FILE: Triangle.java
public non-sealed class Triangle extends Polygon {}

// FILE: Kind.java
public enum Kind implements Shape {
    FLAT {
        @Override public String label() { return "flat"; }
    },
    ROUND;

    public String label() { return "round"; }
}

// FILE: box.kt
class RightTriangle : Triangle()

fun describe(shape: Shape): String = when (shape) {
    is Circle -> "circle"
    is Square -> "square"
    is Triangle -> "triangle"
    Kind.FLAT -> "flat"
    Kind.ROUND -> "round"
}

fun family(shape: Shape): Int = when (shape) {
    is Circle -> 1
    is Polygon -> 2
    is Kind -> 3
}

fun polygon(polygon: Polygon): String = when (polygon) {
    is Square -> "square"
    is Triangle -> "triangle"
}

fun kind(kind: Kind): String = when (kind) {
    Kind.FLAT -> kind.label()
    Kind.ROUND -> kind.label()
}

fun box(): String {
    val read = listOf(Circle(), Square(), RightTriangle(), Kind.FLAT, Kind.ROUND).joinToString(",") {
        describe(it) + family(it)
    }
    if (read != "circle1,square2,triangle2,flat3,round3") return read
    if (polygon(Square()) != "square" || polygon(RightTriangle()) != "triangle") return "polygon"
    if (kind(Kind.FLAT) != "flat" || kind(Kind.ROUND) != "round") return "kind"
    return "OK"
}
