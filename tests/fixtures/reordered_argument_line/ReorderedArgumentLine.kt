class Entry(
    val id: String,
    val name: String,
    val tags: List<String>,
    val note: String? = null,
    val parent: String? = null,
    val size: Int = 0,
)

fun lookup(key: String): String = key + "!"

// The temporary read is coerced to the nullable parameter it fills.
fun coerced(key: String): Entry = Entry(
    id = key,
    name = "n",
    parent = lookup(key),
    tags = listOf(
        key,
    ),
)

// An inlined argument forgets its line before its temporary is stored.
fun inlined(key: String): Entry = Entry(
    id = key,
    name = "n",
    parent = key.trim(),
    tags = listOf(
        key,
    ),
)

// A primitive argument boxed where it is passed, and one passed in place.
fun mixed(key: String, count: Int): Entry = Entry(
    size = count + 1,
    tags = listOf(key),
    name = key.uppercase(),
    id = key,
)

fun box(): String {
    val first = coerced(" a ")
    val second = inlined(" b ")
    val third = mixed("c", 2)
    return if (first.parent == " a !" && second.parent == "b" && third.size == 3 && third.name == "C") "OK" else "fail"
}
