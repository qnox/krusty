package fixture.signatures

actual fun platform(): Int = 1

actual class Box actual constructor() {
    actual fun open(): Int = 2
}
