// LANGUAGE: +CompanionBlocksAndExtensions
// A companion block property that another file reaches first is initialized with its class, and the
// object it holds stays alive through later allocation, though the declaring file never ran its own
// initializer.
// FILE: Main.kt
import holders.Holder

fun box(): String {
    val first = Holder.items
    if (first.size != 3 || first[1] != "item1") return "fail first read: $first"
    var churn = 0
    for (i in 0 until 200_000) {
        churn += "churn$i".length + listOf(i, i + 1).size
    }
    if (churn <= 0) return "fail churn"
    val again = Holder.items
    if (again.size != 3 || again[0] != "item0" || again[2] != "item2") return "fail after churn: $again"
    if (Holder.label.toString() != "OK") return "fail label: ${Holder.label}"
    return Holder.label.toString()
}

// FILE: Holder.kt
package holders

class Holder {
    companion {
        val items = MutableList(3) { "item$it" }
        val label = StringBuilder().append("O").append("K")
    }
}
