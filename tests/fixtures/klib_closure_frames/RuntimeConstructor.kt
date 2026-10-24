package fixture

fun makeWithRuntimeConstructor(seed: Int): () -> Int = {
    val nested = {
        val created = Any()
        seed + 1
    }
    nested()
}
