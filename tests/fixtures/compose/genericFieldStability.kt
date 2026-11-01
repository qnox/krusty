// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A field of a generic class is as stable as that class with the field's type arguments in place of
// its type parameters. A class of the file is inferred again for each set of arguments, and kotlinc
// keeps each result for later classes in declaration order, so `SwapUse` reads the parameters `Swap`
// left unresolved. A class of another file or of a library is read at run time.
// FILE: other.kt
package model

class OBox<T>(val value: T)

// FILE: generic.kt
package model

import androidx.compose.runtime.ProvidedValue
import androidx.compose.runtime.State

class Box<T>(val value: T)
class MBox<T>(var value: T)
class Two<A, B>(val b: B)
class IntBox(val box: Box<Int>)
class ListBox(val box: Box<MutableList<Int>>)
class BoxBox(val box: Box<Box<String>>)
class GenBox<T>(val box: Box<T>)
class MBoxField(val box: MBox<Int>)
class TwoField(val two: Two<MutableList<Int>, Int>)
class TwoField2(val two: Two<Int, MutableList<Int>>)
class Node<T>(val value: T, val next: Node<T>?)
class StarBox(val box: Box<*>)
class OutBox(val box: Box<out Int>)
class NullBox(val box: Box<Int>?)
class FunBox(val box: Box<() -> Unit>)
class Fields(val a: Box<Int>, val b: Box<MutableList<Int>>, val c: Box<Int>)
class Pair2<A, B>(val a: A, val b: B)
class Swap<X, Y>(val pair: Pair2<Y, X>)
class SwapUse(val swap: Swap<String, Int>)
class Holder<T>(val pair: Pair2<T, T>)
class HolderUse(val holder: Holder<Int>)
class Deep<T>(val deep: Box<Box<T>>)
class DeepUse(val deep: Deep<String>)
open class Base<T>(val base: T)
class GenSub<U>(val sub: U) : Base<U>(sub)
class OBoxInt(val box: OBox<Int>)
class OBoxList(val box: OBox<MutableList<Int>>)
class GenOBox<T>(val box: OBox<T>)
class Provided(val value: ProvidedValue<Int>)
class StateField(val state: State<Int>)

// FILE: box.kt
package model

private fun stable(name: String): Int = Class.forName(name).getField("\$stable").getInt(null)

private val expected = listOf(
    "Box" to 0, "MBox" to 8, "Two" to 0, "IntBox" to 0, "ListBox" to 8, "BoxBox" to 8,
    "GenBox" to 0, "MBoxField" to 8, "TwoField" to 0, "TwoField2" to 8, "Node" to 8,
    "StarBox" to 8, "OutBox" to 0, "NullBox" to 0, "FunBox" to 0, "Fields" to 8, "Pair2" to 0,
    "Swap" to 0, "SwapUse" to 8, "Holder" to 0, "HolderUse" to 8, "Deep" to 8, "DeepUse" to 8,
    "Base" to 0, "GenSub" to 8, "GenOBox" to 0, "StateField" to 0,
)

fun box(): String {
    for ((name, bits) in expected) {
        if (stable("model.$name") != bits) return "model.$name: not $bits"
    }
    for (name in listOf("OBoxInt", "OBoxList")) {
        if (stable("model.$name") != stable("model.OBox")) return "model.$name does not read OBox"
    }
    if (stable("model.Provided") != stable("androidx.compose.runtime.ProvidedValue")) {
        return "model.Provided does not read ProvidedValue"
    }
    return "OK"
}
