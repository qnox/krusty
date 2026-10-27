package refused

interface Left {
    fun shared(): Int
    val label: String
}

interface Right {
    fun shared(): Int
    val label: String
}

abstract class Joined : Left, Right
