interface Reading { fun read(): String }

class ValueReading(private val value: String) : Reading {
    override fun read(): String = value
}

class Owner(private val value: String) {
    fun source(): String = value
    fun makeReading(): Reading = ValueReading(value)

    fun delegated(): Reading = object : Reading by makeReading() {}

    fun propertyInitialized(): Reading = object : Reading {
        val initial = source()
        override fun read(): String = initial
    }

    fun blockInitialized(): Reading = object : Reading {
        val initial: String
        init { initial = source() }
        override fun read(): String = initial
    }

    fun memberRetained(): Reading = object : Reading {
        override fun read(): String = source()
    }

    fun lambdaRetained(): Reading = object : Reading {
        val action = { source() }
        override fun read(): String = action()
    }
}

fun box(): String {
    val first = Owner("O")
    val second = Owner("K")
    val observed = first.delegated().read() + second.delegated().read() +
        first.propertyInitialized().read() + second.propertyInitialized().read() +
        first.blockInitialized().read() + second.blockInitialized().read() +
        first.memberRetained().read() + second.memberRetained().read() +
        first.lambdaRetained().read() + second.lambdaRetained().read()
    return if (observed == "OKOKOKOKOK") "OK" else observed
}
