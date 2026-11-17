val top: String = 1
class Owner {
    val member: String = 2
    fun local() {
        val local: String = 3
        var assigned: String = "OK"
        assigned = 4
        val absent: String = null
    }
}
enum class Cases {
    Entry {
        val property: String = 5
    }
}
