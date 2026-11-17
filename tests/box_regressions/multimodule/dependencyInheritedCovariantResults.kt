// MODULE: lib
// FILE: Sources.kt
package fixture

// An abstract library class inherits one member from several interfaces without a body, and the
// results differ only in a type argument. kotlinc selects the declaration whose result is a
// subtype of every other's, by the classifier's declaration-site variance and the result's
// use-site projections. Each body returns the member's result as a `String`, which only the
// selected declaration's result provides.
interface Cov<out T> {
    fun get(): T
}

class Value<T>(private val value: T) : Cov<T> {
    override fun get(): T = value
}

// Declaration-site variance: `Cov<String>` is a subtype of `Cov<CharSequence>` because `Cov`
// declares `out T`, so `NarrowSource.source()` is selected although `WideSource` comes first.
interface WideSource {
    fun source(): Cov<CharSequence>
}

interface NarrowSource {
    fun source(): Cov<String>
}

abstract class Sources : WideSource, NarrowSource {
    fun text(): String = source().get()
}

class Text(private val text: String) : Sources() {
    override fun source(): Cov<String> = Value(text)
}

// Use-site projection: `Box` is invariant, but `Box<String>` is a subtype of the projection
// `Box<out CharSequence>`, so `NarrowBox.box()` is selected although `WideBox` comes first.
class Box<T>(var item: T)

interface WideBox {
    fun box(): Box<out CharSequence>
}

interface NarrowBox {
    fun box(): Box<String>
}

abstract class Boxes : WideBox, NarrowBox {
    fun item(): String = box().item
}

class Boxed(private val text: String) : Boxes() {
    override fun box(): Box<String> = Box(text)
}

// The projection contrast: the first declaration is the more specific one, so it is selected.
interface FirstBox {
    fun box(): Box<String>
}

interface LaterBox {
    fun box(): Box<out CharSequence>
}

abstract class FirstBoxes : FirstBox, LaterBox {
    fun first(): String = box().item
}

class FirstBoxed(private val text: String) : FirstBoxes() {
    override fun box(): Box<String> = Box(text)
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (Text("text").text() != "text") return "text ${Text("text").text()}"
    if (Boxed("item").item() != "item") return "item ${Boxed("item").item()}"
    if (FirstBoxed("first").first() != "first") return "first ${FirstBoxed("first").first()}"
    return "OK"
}
