// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual

import kotlin.reflect.KMutableProperty0
import kotlin.reflect.KMutableProperty1
import kotlin.reflect.KProperty0
import kotlin.reflect.KProperty1

context(c: String, n: Int)
val topProp: String
    get() = c + n

var sink = ""

context(c: String, w: Long)
var topVar: String
    get() = c + w
    set(value) {
        sink = c + value + w
    }

class A(val tag: String) {
    context(c: String)
    val member: String
        get() = tag + c

    context(c: String, d: Double)
    var memberVar: String
        get() = tag + c + d
        set(value) {
            sink = tag + c + value + d
        }
}

context(c: String)
val String.extProp: String
    get() = this + c

context(c: String)
var A.extVar: String
    get() = tag + c
    set(value) {
        sink = tag + c + value
    }

fun box(): String {
    context("A", 1) {
        val p: KProperty0<String> = ::topProp
        if (p.get() != "A1") return "FAIL top: ${p.get()}"
    }
    context("T", 2L) {
        val p: KMutableProperty0<String> = ::topVar
        if (p.get() != "T2") return "FAIL topVar get: ${p.get()}"
        p.set("v")
        if (sink != "Tv2") return "FAIL topVar set: $sink"
    }
    val a = A("t")
    context("X") {
        val bound: KProperty0<String> = a::member
        if (bound.get() != "tX") return "FAIL bound: ${bound.get()}"
        val unbound: KProperty1<A, String> = A::member
        if (unbound.get(a) != "tX") return "FAIL unbound: ${unbound.get(a)}"
        val ext: KProperty0<String> = "R"::extProp
        if (ext.get() != "RX") return "FAIL ext: ${ext.get()}"
        val extUnbound: KProperty1<String, String> = String::extProp
        if (extUnbound.get("Q") != "QX") return "FAIL extUnbound: ${extUnbound.get("Q")}"
        val extVar: KMutableProperty0<String> = a::extVar
        extVar.set("e")
        if (sink != "tXe" || extVar.get() != "tX") return "FAIL extVar: $sink"
        val extVarUnbound: KMutableProperty1<A, String> = A::extVar
        extVarUnbound.set(a, "u")
        if (sink != "tXu") return "FAIL extVarUnbound: $sink"
    }
    context("Y", 2.5) {
        val m: KMutableProperty0<String> = a::memberVar
        m.set("m")
        if (sink != "tYm2.5" || m.get() != "tY2.5") return "FAIL memberVar: $sink"
        val mu: KMutableProperty1<A, String> = A::memberVar
        mu.set(a, "n")
        if (sink != "tYn2.5") return "FAIL memberVarUnbound: $sink"
    }
    return "OK"
}
