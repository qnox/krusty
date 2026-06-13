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

## Leading `00` — RESOLVED
The "extra leading `00`" is the **`UTF8_MODE_MARKER`** (`BitEncoding`): the d1 payload begins with a
`0x00` byte before the delimited `StringTableTypes`. The reader strips it before
`parseDelimitedFrom`. krust emits it verbatim; confirmed by the round-trips below.

## Class metadata (kind=1) — `ProtoBuf.Class`
Reverse-engineered from kotlinc for `class Point(val x: Int, var y: String)` (see
`metadata/class_builder.rs`). `d1 = 00 <delimited StringTableTypes> <Class>`, k=1, mv=[1,9,0], xi=48.

`Class` fields: `f3 = fq_name` (a string-table class-id), `f6 = supertype` (`Type`),
`f8 = constructor` (repeated), `f10 = property` (repeated). Class flags (f1) omitted ⇒ default
(public/final).
- `Type.class_name = f6`.
- `Constructor`: `f2 = value_parameter` (repeated `{f2=name, f3=Type}`), `f100 = JvmMethodSignature`
  ext (`f2 = desc`; name omitted ⇒ `<init>`).
- `Property`: `f2 = name`, `f3 = return_type` (`Type`), `f11 = flags` (emitted as **1798** only for a
  `var`; `val` ⇒ 0, omitted), `f100 = JvmPropertySignature` `{f1 = field (empty ⇒ derived backing
  field), f3 = getter JvmMethodSignature, f4 = setter (var only)}`.
- `JvmMethodSignature`: `f1 = name`, `f2 = desc`.

String table for a class id: `Record.f3 = 2` (operation `DESC_TO_CLASS_ID`) over the descriptor
`Lpkg/Name;`; builtins via `Record.f2 = predefinedIndex`; everything else verbatim. krust emits one
record per string (no range compression) ⇒ semantically equivalent, not byte-identical, to
kotlinc — accepted by the reader, which is the ABI goal.

## Status — round-trips PASSING
Encoding chain ✅, schema + builtin table ✅, `UTF8_MODE_MARKER` ✅. **Both round-trips pass**: a
*Kotlin consumer* compiled by the real kotlinc resolves krust's top-level functions (facade
`@Metadata` + `META-INF/*.kotlin_module`, Phase 5b) **and** uses krust's classes via property syntax
(class `@Metadata` kind=1, Phase 8b). Remaining: richer language surface (data classes, methods in
bodies, generics, nullability) — each extends these same builders.
