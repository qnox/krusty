# `@kotlin.Metadata` — reverse-engineering notes (Phase 4b)

Goal: emit `@kotlin.Metadata` so krust output is consumable as a **Kotlin** library (Java consumers
need only the signatures, already matched in Phase 5a). This is the single largest remaining piece —
effectively a re-implementation of `kotlinx-metadata-jvm`'s writer.

## Reference (kotlinc 1.9.24, `fun f(a: Int): Int = a` in `M.kt` → class `MKt`)

Annotation values: `mv=[1,9,0]`, `k=2` (file facade), `xi=48`, `d2=["f","","a"]`.

`d1` (one string), bytes after byte→char-identity decode (26 bytes):
```
00 08 0a 00 0a 02 10 08 0a 00 1a 0e 10 00 1a 02 30 01 32 06 10 02 1a 02 30 01
```

## Encoding chain (implemented + validated, `metadata/encoding.rs`)
`Package` proto bytes → `bytesToStrings` (byte→char identity; **matches the reference d1 exactly**)
→ modified-UTF-8 in the constant pool. `BitEncoding` default path (`FORCE_8TO7=false`); no marker.

## Decoded structure (proto field numbers from `core/metadata/src/metadata.proto`)
- `Package.function` = field 3 (repeated).
- `Function`: `name`=2 (id in d2), `return_type`=3 (`Type`), `value_parameter`=6 (repeated),
  `flags`=9 (default 6 = public final); JVM ext `method_signature`=100 — **omitted** when the JVM
  descriptor is derivable from the Kotlin signature (the simple case).
- `Type.class_name` = field 6 (fq-name id).
- `ValueParameter`: `name`=2, `type`=3.
- `StringTableTypes` (jvm_metadata.proto): `record`=1 (repeated `Record`); `Record.predefined_index`
  = field 2 → index into `PREDEFINED_STRINGS`.

The reference `Function` (`1a 0e …`) decodes cleanly:
`name=0("f")`, `return_type=Type{class_name=1}`, `value_parameter={name=2("a"), type=Type{class_name=1}}`.
The string table has 3 records: `[{}, {predefinedIndex=8}, {}]` — the empty d2 slot index 1 resolves
to the builtin via `predefinedIndex=8` = `kotlin/Int`.

## Builtin `predefinedIndex` table (`JvmNameResolverBase.PREDEFINED_STRINGS`)
```
0 Any  1 Nothing  2 Unit  3 Throwable  4 Number
5 Byte 6 Double 7 Float 8 Int  9 Long 10 Short 11 Boolean 12 Char
13 CharSequence 14 String  …
```
⇒ krust types: Int→8, Long→9, Double→6, Boolean→11, String→14, Unit→2.

## Open question (the blocker)
`JvmStringTable.serializeTo` = `StringTableTypes.writeDelimitedTo` (varint-len + bytes), then
`Package.writeTo`. That predicts `d1 = 08 <8-byte STT> <Package>`, but the reference has an **extra
leading `00`** (`00 08 …`). Yet the reader is `StringTableTypes.parseDelimitedFrom(in)` then
`Package.parseFrom(in)`, under which a leading `00` would mean an empty table and then a malformed
`Package` (field 1 / field 0). So either the byte→char decode hides a transform, or `writeData`
writes a leading byte not seen in the summarized source. Resolving this needs a live
`kotlin-metadata-jvm` reader to probe, or byte-diffing generated output against the reference under
the Phase 5b round-trip (kotlinc compiling a consumer of krust output).

## Status
Encoding chain ✅, schema + builtin table ✅ (above). The generator (build `Package`/`Function`/
`Type`/`ValueParameter` + `StringTableTypes` + the `@kotlin.Metadata` `RuntimeVisibleAnnotations`
attribute) + the framing fix + the 5b round-trip remain. This is the path; it is large.
