package sample

annotation class Marker

/** Documented. */
@Marker
class Plain {
    interface Inner

    @Marker
    object Holder
}

interface Shape

@Marker object Single

internal annotation class Tag
