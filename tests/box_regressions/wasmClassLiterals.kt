// Class literals over a type and over a value, and the `KClass` members a program reads off
// them. A bound literal answers the receiver's runtime class, never its static type, and a scalar
// receiver answers its box's class. Tokens are equal by the class they stand for.
package regression.classliterals

open class Base
class Derived : Base()
class Outer {
    class Nested
}
object Singleton
interface Marker
enum class Color { RED }

inline fun <reified T : Any> classOf() = T::class

fun box(): String {
    if (Derived::class.simpleName != "Derived") return "fail simpleName"
    if (Derived::class.qualifiedName != "regression.classliterals.Derived") return "fail qualifiedName"
    if (Outer.Nested::class.simpleName != "Nested") return "fail nested simpleName"
    if (Outer.Nested::class.qualifiedName != "regression.classliterals.Outer.Nested") {
        return "fail nested qualifiedName"
    }
    if (Singleton::class.qualifiedName != "regression.classliterals.Singleton") return "fail object"
    if (Marker::class.simpleName != "Marker") return "fail interface"
    if (String::class.qualifiedName != "kotlin.String") return "fail String"
    if (Int::class.simpleName != "Int") return "fail Int"

    val base: Base = Derived()
    if (base::class != Derived::class) return "fail bound"
    if (base::class == Base::class) return "fail bound static type"
    if (classOf<Derived>() != Derived::class) return "fail reified"
    if (classOf<Derived>() == classOf<Base>()) return "fail reified distinct"
    val text: Any = "text"
    if (text::class != String::class) return "fail bound String"
    val number: Any = 42
    if (number::class != Int::class) return "fail bound Int"
    if (number::class == Long::class) return "fail bound Long"
    if (Color.RED::class != Color::class) return "fail enum entry"

    if (Derived::class.hashCode() != base::class.hashCode()) return "fail hashCode"
    // Kotlin/Wasm and Kotlin/Native render `class <qualified name>`; the JVM appends a note when
    // kotlin-reflect is absent.
    val rendered = "${Derived::class}"
    if (rendered != "class regression.classliterals.Derived" &&
        rendered != "class regression.classliterals.Derived (Kotlin reflection is not available)"
    ) {
        return "fail toString $rendered"
    }

    class Local
    if (Local::class.simpleName != "Local") return "fail local simpleName"
    if (Local::class.qualifiedName != null) return "fail local qualifiedName"
    val anonymous = object {}
    if (anonymous::class.simpleName != null) return "fail anonymous simpleName"
    if (anonymous::class == Local::class) return "fail anonymous distinct"
    return "OK"
}
