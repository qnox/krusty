// A member reached through a smart-cast receiver dispatches on the receiver's declared type when the
// narrowed class only inherits that declaration: no cast is needed to call it. A real override in the
// narrowed class, a member of a generic class (inherited through a substitution of its type
// parameters), or a nullable declared receiver still dispatches on the narrowed class.
open class Root {
    open fun root() = 10
}

open class Base<T>(private val item: T) : Root() {
    open fun plain() = 1
    open fun item(): T = item
    open val read: Int get() = 3
    open var stored: Int = 0

    fun implicitCall() = if (this is Leaf) plain() else 0
    fun explicitThisCall() = if (this is Leaf) this.plain() else 0
    fun implicitRead() = if (this is Leaf) read else 0
}

open class Shape {
    open fun area() = 6
    open val sides: Int get() = 4
    open var scale: Int = 1

    fun implicitCall() = if (this is Square) area() else 0
    fun explicitThisCall() = if (this is Square) this.area() else 0
    fun implicitRead() = if (this is Square) sides else 0
    fun explicitThisRead() = if (this is Square) this.sides else 0
    fun implicitWrite(): Int {
        if (this is Square) scale = 7
        return scale
    }
}

class Square : Shape()

open class Leaf : Base<String>("leaf")

class Overriding : Base<String>("over") {
    override fun plain() = 2
}

interface Defaulted {
    fun defaulted() = 5
}

interface Refined : Defaulted

class Plain : Refined

class Holder(val base: Base<String>)

fun local(value: Base<String>) = if (value is Leaf) value.plain() else 0

fun inherited(value: Base<String>) = if (value is Leaf) value.root() else 0

fun fromRoot(value: Root) = if (value is Leaf) value.root() else 0

fun overridden(value: Base<String>) = if (value is Overriding) value.plain() else 0

fun starProjected(value: Base<*>) = if (value is Leaf) value.item().length else 0

fun nullable(value: Base<String>?) = if (value is Leaf) value.plain() else 0

fun anyMember(value: Any) = if (value is Leaf) value.equals(value) else false

fun interfaceDefault(value: Defaulted) = if (value is Plain) value.defaulted() else 0

fun interfaceToInterface(value: Defaulted) = if (value is Refined) value.defaulted() else 0

fun <T : Base<String>> typeParameter(value: T) = if (value is Leaf) value.plain() else 0

fun propertyPath(holder: Holder) = if (holder.base is Leaf) holder.base.plain() else 0

fun inLambda(value: Base<String>) = if (value is Leaf) run { value.plain() } else 0

fun genericRead(value: Base<String>) = if (value is Leaf) value.read else 0

fun read(value: Shape) = if (value is Square) value.sides else 0

fun write(value: Shape): Int {
    if (value is Square) value.scale = 4
    return value.scale
}

fun compound(value: Shape): Int {
    if (value is Square) value.scale += 2
    return value.scale
}

fun shapeCall(value: Shape) = if (value is Square) value.area() else 0

fun box(): String {
    val leaf = Leaf()
    if (local(leaf) != 1) return "fail local"
    if (inherited(leaf) != 10) return "fail inherited"
    if (fromRoot(leaf) != 10) return "fail root"
    if (overridden(Overriding()) != 2) return "fail override"
    if (starProjected(leaf) != 4) return "fail star"
    if (nullable(leaf) != 1 || nullable(null) != 0) return "fail nullable"
    if (!anyMember(leaf)) return "fail any"
    if (interfaceDefault(Plain()) != 5) return "fail default"
    if (interfaceToInterface(Plain()) != 5) return "fail refined"
    if (typeParameter(leaf) != 1) return "fail type parameter"
    if (propertyPath(Holder(leaf)) != 1) return "fail path"
    if (inLambda(leaf) != 1) return "fail lambda"
    if (leaf.implicitCall() != 1 || leaf.explicitThisCall() != 1) return "fail this call"
    if (leaf.implicitRead() != 3 || genericRead(leaf) != 3) return "fail generic read"
    val square = Square()
    if (shapeCall(square) != 6) return "fail shape call"
    if (square.implicitCall() != 6 || square.explicitThisCall() != 6) return "fail shape this call"
    if (square.implicitRead() != 4 || square.explicitThisRead() != 4) return "fail shape this read"
    if (read(square) != 4) return "fail read"
    if (write(square) != 4) return "fail write"
    if (compound(square) != 6) return "fail compound"
    if (square.implicitWrite() != 7) return "fail implicit write"
    if (local(Overriding()) != 0) return "fail miss"
    return "OK"
}
