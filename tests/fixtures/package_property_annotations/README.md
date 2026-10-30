`Properties.kt` compares the complete JVM class inventory, including each property marker and its
metadata signature. Field/getter targeting and suppressed SOURCE provenance remain independent.
The two mixed-clash fixtures keep the same complete marker and delegate declarations in both source
orders, comparing every emitted diagnostic and its location with the reference compiler.

`KlibAnnotations.kt` is the source of `klib-properties.hex`: the two complete property messages from
`default/linkdata/package_store/0_store.knm`, produced by the official Kotlin/Native 2.4.21 compiler
with `kotlinc-native -produce library`. The golden retains both annotation sites, all arguments,
flags, return type references, file references and constant values. Its string constant checks that
the file name interns before annotations and the constant payload after them.
