fun box(): String {
    val observed = IntArray(1)
    val factory = initializedFactory(1) { value ->
        observed[0] = observed[0] * 10 + value
        value
    }
    val first = factory()
    val second = factory()
    return if (first !== second && observed[0] == 123123) "OK" else "wrong initialization"
}
