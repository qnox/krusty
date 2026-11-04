# Closed annotation default argument provenance

`ClosedDefaults.kt` is compiled without changes by Kotlin/Native 2.4.21 using
`kotlinc-native -produce library -o defaults ClosedDefaults.kt`. This release was
selected by `just max-version` when the oracle was recorded.

The complete `Annotation` message nested in `Outer`'s constructor parameter
`ValueParameter.default_value` is:

```text
080112080809120408031004120808081204080310021206080a1202080c
```

Its arguments are `second = 2`, `first = 1`, and the unwritten empty `rest` array,
in that order. The omitted `fallback = 9` declaration default is absent. Integer
values use protobuf zigzag encoding.

`strings.txt` contains the complete fragment string table in index order. The
annotation's classifier is qualified-name index 1 (`Inner`); index 0 is `Outer`.
The lowering test seeds this reference table and those two classifier identities
before comparing the complete nested message and checking that emission leaves
the tables unchanged.

Source SHA-256:
`7736078f68c39dbdd054be585b2eeaf6ba641e094fe0e91f898626c4681cd020`.
