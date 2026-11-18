// A property delegates to a property reference: the standard library's `getValue` and `setValue`
// extensions on `KProperty0`/`KMutableProperty0` and `KProperty1`/`KMutableProperty1` are inline
// one-liners that call `get` and `set`, so reading and writing the delegating property reads and
// writes the referenced one.
class Holder(var count: Int)

var top = "O"

class Owner(val holder: Holder) {
    var mirrored: Int by holder::count
}

var alias: String by ::top

class Fields {
    var text = "a"
}

var Fields.textAlias: String by Fields::text

fun box(): String {
    alias = "OK"
    if (top != "OK") return "FAIL top-level write: $top"
    if (alias != "OK") return "FAIL top-level read: $alias"

    val holder = Holder(1)
    val owner = Owner(holder)
    owner.mirrored = 41
    owner.mirrored += 1
    if (holder.count != 42) return "FAIL member write: ${holder.count}"

    val fields = Fields()
    fields.textAlias = "b"
    if (fields.text != "b" || fields.textAlias != "b") return "FAIL extension: ${fields.text}"

    var local: String by ::top
    local = "local"
    if (top != "local") return "FAIL local write: $top"
    if (local != "local") return "FAIL local read: $local"
    return "OK"
}
