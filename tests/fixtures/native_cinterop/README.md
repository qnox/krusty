# Native cinterop direct-call fixture

`direct_call.klib` is a packed metadata-only interop library generated with Kotlin/Native 2.4.20:

```text
cinterop -def direct_call.def -target linux_x64 -o direct_call
```

The fixture declares one scalar function and one pointer function used to pin a rejected boxed
carrier. Kotlin/Native's producer also writes `default/targets/linux_x64/native/cstubs.bc`; krusty
deliberately consumes only the archive manifest and `linkdata` metadata. The scalar implementation
is compiled independently by the regression, so the test covers the Native provider/backend
contract without adding cinterop generation, library discovery, or linker facts to the compiler.
