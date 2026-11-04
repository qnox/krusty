package fixture

inline fun Int.capture(seed: Int): () -> Int {
    return { seed + this }
}
