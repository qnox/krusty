// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual

import kotlin.jvm.JvmInline
import kotlin.reflect.KMutableProperty1
import kotlin.reflect.KProperty0
import kotlin.reflect.KProperty1

@JvmInline
value class Z(val value: String)

@JvmInline
value class I(val value: Int)

@JvmInline
value class V(val value: String)

var log = ""

context(z: Z)
val maybe: I?
    get() = I(z.value.length)

context(z: Z)
var maybeVar: I?
    get() = I(z.value.length)
    set(value) {
        log += "mv${z.value}${value?.value};"
    }

context(z: Z)
val V.decorated: String
    get() = "[" + z.value + value + "]"

context(z: Z)
var V.decoratedVar: String
    get() = z.value + value
    set(value) {
        log += "dv${z.value}${this.value}$value;"
    }

class Holder(val tag: String) {
    context(z: Z)
    val boxed: I?
        get() = I(tag.length + z.value.length)
}

fun box(): String {
    context(Z("ab")) {
        val maybe: KProperty0<I?> = ::maybe
        if (maybe.get()?.value != 2) return "FAIL maybe: ${maybe.get()}"
        val maybeVar = ::maybeVar
        maybeVar.set(I(5))
        if (maybeVar.get()?.value != 2) return "FAIL maybeVar: ${maybeVar.get()}"
        val decorated: KProperty1<V, String> = V::decorated
        if (decorated.get(V("c")) != "[abc]") return "FAIL decorated: ${decorated.get(V("c"))}"
        val decoratedVar: KMutableProperty1<V, String> = V::decoratedVar
        decoratedVar.set(V("d"), "x")
        if (decoratedVar.get(V("e")) != "abe") return "FAIL decoratedVar: ${decoratedVar.get(V("e"))}"
        val boxed: KProperty1<Holder, I?> = Holder::boxed
        if (boxed.get(Holder("xyz"))?.value != 5) return "FAIL boxed: ${boxed.get(Holder("xyz"))}"
    }
    if (log != "mvab5;dvabdx;") return "FAIL log: $log"
    return "OK"
}
