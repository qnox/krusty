// A safe call proves its receivers non-null only in the branch where the comparison is known to
// hold: `===` and `==` in the then branch, `!==` and `!=` in the else branch. A null receiver
// leaves the safe call null, so the comparison is false for `===`/`==` and true for `!==`/`!=`.
class Holder(val text: String)

fun identityThen(holder: Holder?, value: String): Int =
    if (holder?.text === value) holder.text.length else -1

fun notIdentityElse(holder: Holder?, value: String): Int =
    if (holder?.text !== value) -1 else holder.text.length

fun notEqualsElse(holder: Holder?, value: String): Int =
    if (holder?.text != value) -1 else holder.text.length

fun box(): String {
    val shared = "abc"
    val holder = Holder(shared)
    if (identityThen(holder, shared) != 3) return "fail identityThen"
    if (identityThen(null, shared) != -1) return "fail identityThen null"
    if (notIdentityElse(holder, shared) != 3) return "fail notIdentityElse"
    if (notIdentityElse(null, shared) != -1) return "fail notIdentityElse null"
    if (notEqualsElse(holder, "abc") != 3) return "fail notEqualsElse"
    if (notEqualsElse(null, "abc") != -1) return "fail notEqualsElse null"
    return "OK"
}
