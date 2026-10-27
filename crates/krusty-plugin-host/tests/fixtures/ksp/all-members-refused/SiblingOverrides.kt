package refused

interface Root {
    fun shared(): Int
}

interface First : Root {
    override fun shared(): Int
}

interface Second : Root {
    override fun shared(): Int
}

abstract class Joined : First, Second
