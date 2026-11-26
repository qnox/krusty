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

// A bound: `T` is a subtype of `Base` only through its declared bound `Sub`, so
// `NarrowRead<T>.read()`, whose result `Cov<T>` is a subtype of `Cov<Base>`, is selected although
// `WideRead` comes first. The bound's own supertype `Base` is named by no result or supertype.
open class Base

class Sub(val name: String) : Base()

interface WideRead {
    fun read(): Cov<Base>
}

interface NarrowRead<T : Sub> {
    fun read(): Cov<T>
}

abstract class BoundedReads<T : Sub> : WideRead, NarrowRead<T> {
    fun name(): String = read().get().name
}

class BoundedRead(private val name: String) : BoundedReads<Sub>() {
    override fun read(): Cov<Sub> = Value(Sub(name))
}

// The same bounded result with the narrower declaration first.
abstract class NarrowFirstReads<T : Sub> : NarrowRead<T>, WideRead {
    fun name(): String = read().get().name
}

class NarrowFirstRead(private val name: String) : NarrowFirstReads<Sub>() {
    override fun read(): Cov<Sub> = Value(Sub(name))
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (Text("text").text() != "text") return "text ${Text("text").text()}"
    if (Boxed("item").item() != "item") return "item ${Boxed("item").item()}"
    if (FirstBoxed("first").first() != "first") return "first ${FirstBoxed("first").first()}"
    if (BoundedRead("bound").name() != "bound") return "bound ${BoundedRead("bound").name()}"
    if (NarrowFirstRead("narrow").name() != "narrow") return "narrow ${NarrowFirstRead("narrow").name()}"
    return "OK"
}
