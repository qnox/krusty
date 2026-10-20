use super::builtin_class_access;

/// A `.kotlin_builtins` `Class.flags` word, assembled from the field positions the decoder reads.
fn flags(visibility: u64, modality: u64, kind: u64, is_inner: bool) -> u64 {
    visibility << 1 | modality << 4 | kind << 6 | u64::from(is_inner) << 9
}

/// Every arm has to land on the flags the compiler that produced the mapped `java.*` class file
/// emitted, since the recovered `InnerClasses` entry is compared byte-for-byte against that one.
/// The nested INTERFACE case is the one a JDK-less compile actually hits today
/// (`kotlin/collections/Map.Entry` ⇒ the `0x0609` `java/util/Map$Entry` carries); the rest pin the
/// mapping so a later builtin of another shape cannot silently pick up wrong flags.
#[test]
fn builtin_class_flags_map_to_jvm_access() {
    // public abstract interface, nested (not `inner`) — public static interface abstract.
    assert_eq!(builtin_class_access(flags(3, 2, 1, false)), 0x0609);
    // public final class, nested — public static final (what `MethodHandles$Lookup` carries).
    assert_eq!(builtin_class_access(flags(3, 0, 0, false)), 0x0019);
    // The same class declared `inner`: an inner class is not static.
    assert_eq!(builtin_class_access(flags(3, 0, 0, true)), 0x0011);
    // public abstract class — abstract survives, final does not appear.
    assert_eq!(builtin_class_access(flags(3, 2, 0, false)), 0x0409);
    // `sealed` is abstract on the JVM.
    assert_eq!(builtin_class_access(flags(3, 3, 0, false)), 0x0409);
    // public enum class — public static final enum.
    assert_eq!(builtin_class_access(flags(3, 0, 2, false)), 0x4019);
    // public annotation class — an annotation interface.
    assert_eq!(builtin_class_access(flags(3, 2, 4, false)), 0x2609);
    // private and protected keep their own bit; `internal` is PUBLIC on the JVM (kotlinc mangles
    // the NAME instead of narrowing the flag), so it must not come out package-private.
    assert_eq!(builtin_class_access(flags(1, 0, 0, false)), 0x001a);
    assert_eq!(builtin_class_access(flags(2, 0, 0, false)), 0x001c);
    assert_eq!(builtin_class_access(flags(0, 0, 0, false)), 0x0019);
    // `open` is neither final nor abstract.
    assert_eq!(builtin_class_access(flags(3, 1, 0, false)), 0x0009);
}

/// The arm assertions above assemble the flag word with the very shifts the decoder reads back, so
/// they pin the MAPPING but say nothing about the OFFSETS — moving `IS_INNER` a bit over would keep
/// them all green. These are `Class.flags` words as they actually appear in the stdlib's shipped
/// `.kotlin_builtins` fragments, so they pin the layout itself.
#[test]
fn real_builtins_flag_words_decode_to_jvm_access() {
    // `kotlin/collections/Map.Entry` — public abstract interface, not `inner`. Anchors VISIBILITY
    // and CLASS_KIND, and is the one word a JDK-less compile actually decodes today; the entry it
    // produces is compared byte-for-byte against `java/util/Map$Entry`'s in
    // `tests/no_jdk_builtin_emit_e2e.rs`.
    assert_eq!(builtin_class_access(0x0066), 0x0609);
    // `kotlin/Any` — public OPEN class. Its modality is the one that pins MODALITY at bits 4..6:
    // read one bit over and it would decode as ABSTRACT or FINAL.
    assert_eq!(builtin_class_access(0x0016), 0x0009);
    // The protobuf default for an omitted `Class.flags` field is PUBLIC FINAL (`6`). The parser
    // supplies that word before this decoder runs; it must land on public static final.
    assert_eq!(builtin_class_access(6), 0x0019);
    // An EXPLICIT zero is INTERNAL FINAL. Internal classes are public in JVM bytecode because
    // Kotlin enforces the visibility through metadata/name mangling rather than the ACC bits.
    assert_eq!(builtin_class_access(0), 0x0019);
}
