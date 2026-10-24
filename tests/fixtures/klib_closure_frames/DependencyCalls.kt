package fixture

class BodyOnly(val value: Int)

fun makeWithConstructor(seed: Int): () -> Int = {
    val nested = {
        val created = BodyOnly(seed)
        seed + 1
    }
    nested()
}
