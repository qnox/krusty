//! kotlinc's `IteratorNext` intrinsic calls a `kotlin.collections` primitive iterator's unboxed
//! element operation (`IntIterator.nextInt()I`) for a call of that iterator's own `next()`, in a
//! source call as in a `for` loop over an `operator fun iterator(): IntIterator`. A boxed use boxes
//! the element afterwards; a generic `Iterator<Int>` keeps the interface `next()` and its cast.
use super::common;

const SOURCE: &str = "package store\n\
               \n\
               fun sink(x: Int) {}\n\
               fun sinkC(x: Char) {}\n\
               \n\
               fun ints(it: IntIterator) {\n\
               \x20   while (it.hasNext()) sink(it.next())\n\
               }\n\
               fun longs(it: LongIterator): Long = it.next()\n\
               fun chars(it: CharIterator) { sinkC(it.next()) }\n\
               fun doubles(it: DoubleIterator): Double = it.next()\n\
               fun floats(it: FloatIterator): Float = it.next()\n\
               fun bytes(it: ByteIterator): Byte = it.next()\n\
               fun shorts(it: ShortIterator): Short = it.next()\n\
               fun booleans(it: BooleanIterator): Boolean = it.next()\n\
               fun boxed(it: IntIterator): Any = it.next()\n\
               fun nullable(it: IntIterator?): Int? = it?.next()\n\
               fun generic(it: Iterator<Int>): Int = it.next()\n\
               \n\
               class Holder(val it: IntIterator) {\n\
               \x20   operator fun iterator(): IntIterator = it\n\
               }\n\
               \n\
               fun loop(h: Holder) {\n\
               \x20   for (x in h) sink(x)\n\
               }\n";

#[test]
fn primitive_iterator_next_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "PrimitiveIteratorNext",
        SOURCE,
        "store/PrimitiveIteratorNextKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("store/PrimitiveIteratorNextKt byte-identical to kotlinc");
}

// PLATFORM-SPECIFIC: the existing whole source pins Native object emission for every primitive
// bridge and its boxed/nullable/interface uses; the JVM test above owns its bytecode oracle.
#[test]
fn primitive_iterator_bridges_emit_verified_native_objects() {
    use common::native_backend::{native_objects, NativeBox};
    let target = krusty::native::NativeTarget::ALL[0];
    let objects = match native_objects(&[("PrimitiveIteratorNext", SOURCE)], target) {
        Ok(objects) => objects,
        Err(NativeBox::Declined(reason) | NativeBox::Failed(reason)) => panic!("{reason}"),
        Err(NativeBox::Unavailable) => panic!("the pinned stdlib must be provisioned"),
        Err(_) => panic!("object emission must not execute the program"),
    };
    assert!(
        !objects.is_empty(),
        "the complete source must emit an object"
    );
}
