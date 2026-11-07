package overridees

// A member of a library classifier with type parameters is not described on its own yet.
class Version : Comparable<Version> {
    override fun compareTo(other: Version): Int = 0
}
