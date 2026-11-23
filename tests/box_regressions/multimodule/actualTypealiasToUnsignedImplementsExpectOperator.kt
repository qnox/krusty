// LANGUAGE: +MultiPlatformProjects +AllowExpectValueClassesWithNoPrimaryConstructor
// WITH_STDLIB
// An `expect` operator is implemented by the dependency member an `actual typealias` reaches even
// when the dependency declares that member `inline`, as the Native library does for `UInt`.

// MODULE: common
expect value class CommonUSize : Comparable<CommonUSize> {
    override operator fun compareTo(other: CommonUSize): Int
}

fun compareUSize(a: CommonUSize, b: CommonUSize) = a.compareTo(b)

// MODULE: platform()()(common)
actual typealias CommonUSize = UInt

fun box() = if (compareUSize(20.toUInt(), 30.toUInt()) < 0) "OK" else "fail"
