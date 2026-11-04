`Properties.kt` compares the complete JVM class inventory, including each property marker and its
metadata signature. Field/getter targeting and suppressed SOURCE provenance remain independent.
`UseSites.kt` compares every complete class for isolated getter, setter, setter-parameter, field
and property destinations, mixed destinations and SOURCE retention, using ordinary user annotations.
`SetterPayloads.kt` independently checks complete default-setter parameter payloads with mixed
retention, nested annotations, arrays and nullable reference storage.
The two mixed-clash fixtures keep the same complete marker and delegate declarations in both source
orders, comparing every emitted diagnostic and its location with the reference compiler.

`KlibAnnotations.kt` is the source of `klib-properties.hex`: the two complete property messages from
`default/linkdata/package_store/0_store.knm`, produced by the official Kotlin/Native 2.4.21 compiler
with `kotlinc-native -produce library`. The golden retains both annotation sites, all arguments,
flags, return type references, file references and constant values. Its string constant checks that
the file name interns before annotations and the constant payload after them.
