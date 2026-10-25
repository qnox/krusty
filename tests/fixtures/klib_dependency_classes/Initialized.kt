package fixture

class Initialized(seed: Int, effect: (Int) -> Int) {
    private val first = effect(seed)
    private val tag = "stored"
    private val second = effect(seed + 1)

    init {
        effect(seed + 2)
    }
}

fun initializedFactory(seed: Int, effect: (Int) -> Int): () -> Initialized = {
    Initialized(seed, effect)
}
