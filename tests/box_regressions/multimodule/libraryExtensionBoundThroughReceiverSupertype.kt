// MODULE: lib
// FILE: Lib.kt
import kotlin.reflect.KClass

interface Poly<T>

open class Item

open class Special : Item() {
    var mark = 0
}

interface Items : Poly<Item>

class Registration<out C, U>(val container: C, val item: U)

// `T` is fixed only by the receiver's `Poly<T>` supertype: `U : T` must not make it `U`'s actual.
@Suppress("UNCHECKED_CAST")
fun <T : Any, C : Poly<T>, U : T> C.registering(
    type: KClass<U>,
    configuration: U.() -> Unit,
): Registration<out C, U> = Registration(this, Special().also { (it as U).configuration() } as U)

// MODULE: main(lib)
// FILE: Main.kt
fun box(): String {
    val items = object : Items {}
    val registered = items.registering(Special::class) { mark = 7 }
    return if (registered.item.mark == 7 && registered.container === items) "OK" else "fail"
}
