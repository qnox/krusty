// A property reference is a `KProperty` value, and `toString` prints the property it names. A
// generic class reads its own property through `KProperty1.get`, so a `toString` of that class's
// type parameter may reach the reference object as well as the strings it holds here.
import kotlin.reflect.KProperty0
import kotlin.reflect.KProperty1

class Box<T>(var content: T) {
    fun viaMember(): T = Box<T>::content.get(this)

    fun viaBound(): T = this::content.get()
}

val top = 1

fun <T> show(box: Box<T>): String = "" + box.viaMember() + box.viaBound()

fun <T> describe(value: T): String = value.toString()

fun box(): String {
    if (show(Box("O")) != "OO") return "FAIL generic read: ${show(Box("O"))}"
    val member: KProperty1<Box<String>, String> = Box<String>::content
    if (describe(member) != "property content (Kotlin reflection is not available)") {
        return "FAIL member toString: ${describe(member)}"
    }
    val bound: KProperty0<Int> = ::top
    if ("$bound" != "property top (Kotlin reflection is not available)") {
        return "FAIL top-level toString: $bound"
    }
    return "OK"
}
