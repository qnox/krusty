//! The native runtime's object model, allocator and collector, exercised from C.
//!
//! The collector's interesting cases — cycles, interior pointers, a reference reachable only from
//! the heap — are easiest to arrange from C, where the test can hold or drop a root exactly when it
//! means to. So this test compiles a hand-written C program against `krusty_rt.h` and links it with
//! krusty's OWN linker against the SAME prebuilt runtime objects every Kotlin program links: the
//! program supplies `kt_program_entry` itself, in place of the entry the code generator emits for
//! a Kotlin `main`, and exits with a distinct nonzero code per failed assertion so a failure names
//! the property that broke.
//!
//! Nothing here inspects generated code. A collector that looks right and frees a live object is
//! the failure a text assertion cannot see.
//!
//! Skips rather than fails when there is no C compiler for the test program, or when this build of
//! krusty carries no prebuilt runtime for the host.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::native::NativeTarget;

use super::common;

/// A scratch directory that cleans itself up.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "krusty-native-gc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The C compiler for the TEST PROGRAM only — the runtime itself was compiled when krusty was
/// built. `$KRUSTY_RUNTIME_CC` is what `build.rs` honours, so the same compiler serves both.
fn c_compiler() -> Option<String> {
    if let Ok(compiler) = std::env::var("KRUSTY_RUNTIME_CC") {
        if !compiler.trim().is_empty() {
            return Some(compiler);
        }
    }
    ["clang", "cc", "gcc"].into_iter().find_map(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| candidate.to_string())
    })
}

/// The host target, when this build can link for it and a C compiler exists for the test program.
fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty::native::can_link(target) && c_compiler().is_some()).then_some(target)
}

/// Compile the C program to a freestanding object with the runtime's own flags — except at `-O0`,
/// as the C path always compiled it: the program drops its roots by overwriting locals, which an
/// optimizer is free to keep in a register the conservative scanner then finds.
fn compile_c(scratch: &Path, source: &str) -> Vec<u8> {
    let compiler = c_compiler().expect("checked by `available`");
    std::fs::write(
        scratch.join("krusty_sys.h"),
        krusty::native::runtime::SYS_HEADER,
    )
    .expect("write header");
    std::fs::write(scratch.join("krusty_rt.h"), krusty::native::runtime::HEADER)
        .expect("write header");
    let program = scratch.join("gc_test.c");
    std::fs::write(&program, source).expect("write program");
    let object = scratch.join("gc_test.o");
    let output = Command::new(&compiler)
        .args([
            "-std=c11",
            "-ffreestanding",
            "-nostdlib",
            "-fno-pic",
            "-fno-stack-protector",
            "-O0",
            "-c",
            "-o",
        ])
        .arg(&object)
        .arg(&program)
        .output()
        .expect("run the C compiler");
    assert!(
        output.status.success(),
        "the test program must compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&object).expect("read object")
}

/// What each nonzero exit code of the C program means. The program and this table are written
/// together; a code missing here is a bug in the test, not in the runtime.
const FAILURES: &[(i32, &str)] = &[
    (11, "unrooted objects were still live after a collection"),
    (
        12,
        "a rooted object was freed or its contents were damaged by a collection",
    ),
    (
        13,
        "a linked list rooted only at its head lost nodes: heap tracing missed a reference field",
    ),
    (
        14,
        "an unrooted cycle survived a collection: mark-sweep must not need a reference count",
    ),
    (15, "an object rooted only by an interior pointer was freed"),
    (
        16,
        "an object referenced only from a non-reference (kt_long) field survived: heap tracing \
          must be precise, not conservative",
    ),
    (
        17,
        "swept objects were not reused: the heap grew while free slots existed",
    ),
    (18, "a rooted large object was freed or damaged"),
    (19, "an unrooted large object was not reclaimed"),
    (
        20,
        "an unrooted large object's mapping was not returned: heap bytes did not shrink",
    ),
    (
        21,
        "automatic collection did not bound the heap under a stream of garbage",
    ),
    (
        22,
        "no string object was live after building one across collections (damaged TEXT shows up \
          as a stdout mismatch instead: the string's byte array is not traced)",
    ),
    (
        23,
        "the live-object count disagreed with what the program holds",
    ),
    (
        24,
        "a rooted reference array lost or damaged an element: an array's elements must be traced \
          as precisely as a field is",
    ),
    (
        25,
        "an unrooted reference array's elements survived: an array holds its elements alive, and \
          nothing else does",
    ),
    (
        26,
        "objects referenced only from a LongArray's elements survived: an array of the same \
          element WIDTH must not be walked as pointers",
    ),
    (
        27,
        "a registered global root was freed or damaged: static slots are not discovered, they are \
          registered",
    ),
    (
        28,
        "a reference into static storage did not survive a collection untouched: an address that \
          belongs to no heap chunk must be ignored, not swept",
    ),
    (
        29,
        "an object reachable only through an array reachable only through a field was freed: \
          tracing must follow arrays as it follows objects",
    ),
    (
        30,
        "a mixed live set did not come through repeated collections intact",
    ),
];

fn describe(code: i32) -> String {
    FAILURES
        .iter()
        .find(|(known, _)| *known == code)
        .map_or_else(
            || format!("exit code {code}, which the C program does not define"),
            |(_, meaning)| format!("exit code {code}: {meaning}"),
        )
}

/// The C program. Read alongside [`FAILURES`].
///
/// Every check that expects something to be RECLAIMED allocates it in a separate `noinline`
/// function and clobbers the stack below the caller before collecting: roots are found
/// conservatively, so a dead pointer left in a stale stack slot or a scratch register would keep
/// the object alive and make the check flaky. The clobber is what makes exact counts safe to
/// assert. `-O0` (what the linker passes) keeps every local in its own stack slot, so nothing
/// survives in a callee-saved register across these calls either.
const PROGRAM: &str = r#"
#include "krusty_rt.h"

/* ---- test types: each begins with the object header and names its reference fields ---------- */

typedef struct Node {
    KObjectHeader header;
    struct Node *next;
    kt_long value;
} Node;
static const uint32_t node_references[] = {offsetof(Node, next)};
static const KType node_type = {"Node", 4, sizeof(Node), 1, 0, node_references};

/* No reference fields at all: `address` holds a pointer's BITS in a kt_long, which a precise
   tracer must not follow. */
typedef struct Leaf {
    KObjectHeader header;
    kt_long value;
    kt_long address;
} Leaf;
static const KType leaf_type = {"Leaf", 4, sizeof(Leaf), 0, 0, NULL};

/* Larger than the largest small size class, so it takes the large-object path. */
typedef struct Big {
    KObjectHeader header;
    kt_long values[512];
} Big;
static const KType big_type = {"Big", 3, sizeof(Big), 0, 0, NULL};

static void fail(kt_int code) { kt_exit(code); }

/* Overwrite the stack region below the caller, where the frames of functions that have already
   returned — and any references they held — still lie. */
__attribute__((noinline)) static void clobber_stack(void) {
    volatile uintptr_t scratch[2048];
    for (size_t i = 0; i < 2048; i++) {
        scratch[i] = 0;
    }
}

__attribute__((noinline)) static void allocate_garbage(uint32_t count) {
    for (uint32_t i = 0; i < count; i++) {
        Leaf *leaf = (Leaf *)kt_gc_allocate(&leaf_type, sizeof(Leaf));
        leaf->value = i;
    }
}

/* 1. Nothing rooted, everything reclaimed. */
__attribute__((noinline)) static void test_unrooted_objects_are_reclaimed(void) {
    allocate_garbage(10000);
    clobber_stack();
    kt_gc_collect();
    if (kt_gc_live_objects() != 0) {
        fail(11);
    }
}

/* 2. Rooted objects survive with their contents intact, garbage between them does not. */
__attribute__((noinline)) static void test_rooted_objects_survive_intact(void) {
    Leaf *kept[100];
    for (uint32_t i = 0; i < 100; i++) {
        allocate_garbage(50);
        kept[i] = (Leaf *)kt_gc_allocate(&leaf_type, sizeof(Leaf));
        kept[i]->value = (kt_long)i * 7 + 1;
    }
    clobber_stack();
    kt_gc_collect();
    for (uint32_t i = 0; i < 100; i++) {
        if (kept[i]->value != (kt_long)i * 7 + 1) {
            fail(12);
        }
        if (kept[i]->header.type != &leaf_type) {
            fail(12);
        }
    }
    if (kt_gc_live_objects() != 100) {
        fail(23);
    }
}

__attribute__((noinline)) static Node *build_list(uint32_t length) {
    Node *head = NULL;
    for (uint32_t i = 0; i < length; i++) {
        Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
        node->value = (kt_long)(length - 1 - i);
        node->next = head;
        head = node;
    }
    return head;
}

/* 3. Only the head is rooted; every node is reachable through `next`, and `next` is the one field
   the type lists as a reference. */
__attribute__((noinline)) static void test_a_list_is_traced_through_its_reference_fields(void) {
    Node *head = build_list(1000);
    clobber_stack();
    kt_gc_collect();
    kt_long expected = 0;
    for (Node *node = head; node != NULL; node = node->next) {
        if (node->value != expected) {
            fail(13);
        }
        expected++;
    }
    if (expected != 1000 || kt_gc_live_objects() != 1000) {
        fail(13);
    }
}

__attribute__((noinline)) static void build_cycle(void) {
    Node *a = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    Node *b = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    a->next = b;
    b->next = a;
}

/* 4. A cycle with no root is garbage. */
__attribute__((noinline)) static void test_an_unrooted_cycle_is_reclaimed(void) {
    build_cycle();
    clobber_stack();
    kt_gc_collect();
    if (kt_gc_live_objects() != 0) {
        fail(14);
    }
}

/* Returns the address of a FIELD; the object's own address dies with this frame. */
__attribute__((noinline)) static kt_long *allocate_and_point_inside(void) {
    Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    node->value = 424242;
    return &node->value;
}

/* 5. A C compiler may keep a pointer to a field rather than to the object; that must root it. */
__attribute__((noinline)) static void test_an_interior_pointer_roots_its_object(void) {
    kt_long *field = allocate_and_point_inside();
    clobber_stack();
    kt_gc_collect();
    if (*field != 424242 || kt_gc_live_objects() != 1) {
        fail(15);
    }
}

__attribute__((noinline)) static void hide_a_pointer_in_a_long(Leaf *outer) {
    Leaf *inner = (Leaf *)kt_gc_allocate(&leaf_type, sizeof(Leaf));
    inner->value = 7;
    outer->address = (kt_long)(uintptr_t)inner;
}

/* 6. The bits of a pointer in a kt_long field are not a reference. A conservative heap scan would
   keep `inner`; a precise one, reading `leaf_type`'s (empty) reference list, frees it. */
__attribute__((noinline)) static void test_heap_tracing_is_precise(void) {
    Leaf *outer = (Leaf *)kt_gc_allocate(&leaf_type, sizeof(Leaf));
    hide_a_pointer_in_a_long(outer);
    clobber_stack();
    kt_gc_collect();
    if (kt_gc_live_objects() != 1 || outer->address == 0) {
        fail(16);
    }
}

/* 7. Sweeping frees for REUSE: a second batch the size of a freed first batch maps nothing new. */
__attribute__((noinline)) static void test_swept_slots_are_reused(void) {
    allocate_garbage(10000);
    clobber_stack();
    kt_gc_collect();
    size_t before = kt_gc_heap_bytes();
    allocate_garbage(10000);
    if (kt_gc_heap_bytes() != before) {
        fail(17);
    }
    clobber_stack();
    kt_gc_collect();
}

__attribute__((noinline)) static Big *allocate_big(void) {
    Big *big = (Big *)kt_gc_allocate(&big_type, sizeof(Big));
    for (uint32_t i = 0; i < 512; i++) {
        big->values[i] = (kt_long)i * 3;
    }
    return big;
}

/* 8. Large objects: rooted, they survive a collection intact; unrooted, they are reclaimed and
   their mapping is returned, so the heap shrinks. */
__attribute__((noinline)) static void test_large_objects(void) {
    size_t before = kt_gc_heap_bytes();
    Big *big = allocate_big();
    if (kt_gc_heap_bytes() <= before) {
        fail(18); /* a large object must have mapped something */
    }
    size_t with_big = kt_gc_heap_bytes();
    kt_gc_collect();
    for (uint32_t i = 0; i < 512; i++) {
        if (big->values[i] != (kt_long)i * 3) {
            fail(18);
        }
    }
    if (kt_gc_live_objects() != 1) {
        fail(18);
    }
    big = NULL;
    clobber_stack();
    kt_gc_collect();
    if (kt_gc_live_objects() != 0) {
        fail(19);
    }
    if (kt_gc_heap_bytes() >= with_big) {
        fail(20);
    }
}

/* 9. The automatic trigger: a stream of garbage far larger than the heap must not grow it
   without bound. 1,000,000 leaves is 32 MB of allocation; the heap stays a small multiple of the
   collector's minimum cycle. */
__attribute__((noinline)) static void test_automatic_collection_bounds_the_heap(void) {
    allocate_garbage(1000000);
    if (kt_gc_heap_bytes() > 16u * 1024u * 1024u) {
        fail(21);
    }
    clobber_stack();
    kt_gc_collect();
}

/* 10. Strings: the text lives in a separate heap object the string's type lists as a reference.
   Building one across many collections and printing it afterwards checks that field is traced
   — and that a literal (static storage) and the Unit singleton pass through a collection
   untouched, since neither lives in the heap. The Rust side compares the output. */
__attribute__((noinline)) static void test_strings_survive_collections(void) {
    KRef literal = kt_string_utf8("literal", 7);
    KRef text = kt_string_utf8("", 0);
    for (kt_int i = 0; i < 300; i++) {
        text = kt_string_plus(text, kt_box_int(i % 10));
        if (i % 50 == 0) {
            clobber_stack();
            kt_gc_collect();
        }
    }
    kt_gc_collect();
    kt_println_any(text);
    kt_println_any(literal);
    kt_println_any(kt_unit());
    kt_println_any(NULL);
    if (kt_gc_live_objects() == 0) {
        fail(22);
    }
}

/* 11. An array of references is traced through its ELEMENTS, which the descriptor describes with a
   stride and a flag rather than with a fixed offset table — an array's length is not known when its
   type is written. A rooted array must keep every element alive and intact. */
__attribute__((noinline)) static void test_a_reference_array_is_traced(void) {
    KRef array = kt_array_new(&kt_type_array, 64);
    for (kt_int i = 0; i < 64; i++) {
        Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
        node->value = 1000 + i;
        ((KRef *)((char *)array + kt_type_array.instance_size))[i] = (KRef)node;
    }
    allocate_garbage(20000);
    clobber_stack();
    kt_gc_collect();
    for (kt_int i = 0; i < 64; i++) {
        Node *node =
            (Node *)((KRef *)((char *)array + kt_type_array.instance_size))[i];
        if (node == NULL || node->value != 1000 + i) {
            fail(24);
        }
    }
}

/* 12. The other half of the same property: the array is the ONLY thing holding those elements, so
   when it goes they go. A tracer that marked every allocated object would pass test 11 and fail
   this one. */
__attribute__((noinline)) static void build_unrooted_array(void) {
    KRef array = kt_array_new(&kt_type_array, 32);
    for (kt_int i = 0; i < 32; i++) {
        Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
        node->value = i;
        ((KRef *)((char *)array + kt_type_array.instance_size))[i] = (KRef)node;
    }
}

__attribute__((noinline)) static void test_an_unrooted_reference_array_is_reclaimed(void) {
    kt_gc_collect();
    size_t before = kt_gc_live_objects();
    /* Built in a frame that has RETURNED: with conservative roots, a reference the current frame
       ever held may still sit in a register or a stack slot the scanner reads, and "unreachable"
       has to mean unreachable to the scanner too, not just to the program. */
    build_unrooted_array();
    clobber_stack();
    kt_gc_collect();
    if (kt_gc_live_objects() != before) {
        fail(25);
    }
}

/* 13. A `LongArray`'s elements are exactly as wide as a reference and may hold a pointer's BITS.
   The descriptor says they are not references, and that has to be believed: the objects those bits
   name are unreachable and must be reclaimed. This is the array form of test 6. */
__attribute__((noinline)) static void fill_with_pointer_bits(KRef bits) {
    for (kt_int i = 0; i < 16; i++) {
        Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
        node->value = i;
        ((kt_long *)((char *)bits + kt_type_long_array.instance_size))[i] = (kt_long)(uintptr_t)node;
    }
}

__attribute__((noinline)) static void test_array_tracing_is_precise(void) {
    kt_gc_collect();
    size_t before = kt_gc_live_objects();
    KRef bits = kt_array_new(&kt_type_long_array, 16);
    fill_with_pointer_bits(bits);
    clobber_stack();
    kt_gc_collect();
    /* The array itself is rooted here; the 16 nodes it names only in integer form must go. */
    if (kt_gc_live_objects() != before + 1) {
        fail(26);
    }
    for (kt_int i = 0; i < 16; i++) {
        if (((kt_long *)((char *)bits + kt_type_long_array.instance_size))[i] == 0) {
            fail(26);
        }
    }
}

/* 14. A global slot. A freestanding program cannot find its own data section, so a static slot that
   may hold a reference is REGISTERED rather than discovered; this is what every top-level property
   and every `object` singleton depends on. `holder` deliberately never appears on the stack after
   registration: the root must be the registration, not a leftover copy. */
static KRef global_slot;

__attribute__((noinline)) static void fill_global_slot(void) {
    Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    node->value = 4242;
    Node *tail = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    tail->value = 2424;
    node->next = tail;
    global_slot = (KRef)node;
}

__attribute__((noinline)) static void test_a_global_root_keeps_its_object(void) {
    kt_gc_add_global_root((void **)&global_slot);
    fill_global_slot();
    clobber_stack();
    allocate_garbage(20000);
    clobber_stack();
    kt_gc_collect();
    Node *node = (Node *)global_slot;
    if (node == NULL || node->value != 4242 || node->next == NULL || node->next->value != 2424) {
        fail(27);
    }
}

/* 15. The box cache and every string literal live in STATIC storage, outside the heap. A collection
   must leave them alone — and a heap object whose field points at one must survive tracing rather
   than trip over an address that belongs to no chunk. */
__attribute__((noinline)) static void test_static_references_survive_untouched(void) {
    KRef cached = kt_box_int(7);
    KRef again = kt_box_int(7);
    if (cached != again) {
        fail(28); /* the cache is what makes this a static-storage test at all */
    }
    Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    node->next = (Node *)cached; /* a heap object pointing OUT of the heap */
    node->value = 99;
    allocate_garbage(20000);
    clobber_stack();
    kt_gc_collect();
    if (node->value != 99 || (KRef)node->next != cached) {
        fail(28);
    }
    if (kt_unbox_int(cached) != 7 || kt_box_int(7) != cached) {
        fail(28);
    }
}

/* 16. Depth: an object held only by an array held only by a field. Nothing here is on the stack but
   the head, so every edge has to be followed — object to field, field to array, array to element. */
__attribute__((noinline)) static void test_tracing_follows_arrays_inside_objects(void) {
    Node *head = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    head->value = 1;
    {
        KRef array = kt_array_new(&kt_type_array, 8);
        for (kt_int i = 0; i < 8; i++) {
            Node *leaf = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
            leaf->value = 500 + i;
            ((KRef *)((char *)array + kt_type_array.instance_size))[i] = (KRef)leaf;
        }
        head->next = (Node *)array;
    }
    clobber_stack();
    allocate_garbage(20000);
    clobber_stack();
    kt_gc_collect();
    KRef array = (KRef)head->next;
    for (kt_int i = 0; i < 8; i++) {
        Node *leaf = (Node *)((KRef *)((char *)array + kt_type_array.instance_size))[i];
        if (leaf == NULL || leaf->value != 500 + i) {
            fail(29);
        }
    }
}

/* 17. Everything at once, repeatedly: a live set of several object kinds and sizes — small, large,
   arrays, strings, a global root — with garbage churned between collections. Each round verifies
   every live value, so a collector that is right about one kind and wrong about another in
   combination shows up here rather than in production. */
static KRef stress_slot;

__attribute__((noinline)) static void test_a_mixed_live_set_survives_many_collections(void) {
    kt_gc_add_global_root((void **)&stress_slot);
    KRef array = kt_array_new(&kt_type_array, 16);
    Node *chain = NULL;
    for (kt_int i = 0; i < 16; i++) {
        Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
        node->value = 7000 + i;
        node->next = chain;
        chain = node;
        ((KRef *)((char *)array + kt_type_array.instance_size))[i] = (KRef)node;
    }
    Big *big = (Big *)kt_gc_allocate(&big_type, sizeof(Big));
    big->values[0] = 31337;
    big->values[511] = 73313;
    stress_slot = kt_string_utf8("mixed", 5);

    for (kt_int round = 0; round < 12; round++) {
        allocate_garbage(5000);
        {
            KRef scratch = kt_array_new(&kt_type_array, 64);
            (void)scratch;
        }
        clobber_stack();
        kt_gc_collect();

        for (kt_int i = 0; i < 16; i++) {
            Node *node = (Node *)((KRef *)((char *)array + kt_type_array.instance_size))[i];
            if (node == NULL || node->value != 7000 + i) {
                fail(30);
            }
        }
        kt_int length = 0;
        for (Node *node = chain; node != NULL; node = node->next) {
            length++;
        }
        if (length != 16) {
            fail(30);
        }
        if (big->values[0] != 31337 || big->values[511] != 73313) {
            fail(30);
        }
        if (stress_slot == NULL) {
            fail(30);
        }
    }
    kt_println_any(stress_slot);
}

void kt_program_entry(void) {
    kt_long stack_anchor = 0;
    kt_runtime_init(&stack_anchor);

    test_unrooted_objects_are_reclaimed();
    clobber_stack();
    test_rooted_objects_survive_intact();
    clobber_stack();
    test_a_list_is_traced_through_its_reference_fields();
    clobber_stack();
    test_an_unrooted_cycle_is_reclaimed();
    clobber_stack();
    test_an_interior_pointer_roots_its_object();
    clobber_stack();
    test_heap_tracing_is_precise();
    clobber_stack();
    test_swept_slots_are_reused();
    clobber_stack();
    test_large_objects();
    clobber_stack();
    test_automatic_collection_bounds_the_heap();
    clobber_stack();
    test_strings_survive_collections();
    clobber_stack();
    test_a_reference_array_is_traced();
    clobber_stack();
    test_an_unrooted_reference_array_is_reclaimed();
    clobber_stack();
    test_array_tracing_is_precise();
    clobber_stack();
    test_a_global_root_keeps_its_object();
    clobber_stack();
    test_static_references_survive_untouched();
    clobber_stack();
    test_tracing_follows_arrays_inside_objects();
    clobber_stack();
    test_a_mixed_live_set_survives_many_collections();
    kt_exit(0);
}
"#;

#[test]
fn the_collector_reclaims_garbage_and_keeps_what_is_reachable() {
    let Some(target) = host() else {
        eprintln!("skipping: needs a C compiler and a prebuilt native runtime for the host");
        return;
    };
    // The program links against exactly what a Kotlin program links against — the prebuilt
    // runtime, through krusty's linker — only the entry point is the program's own.
    let scratch = Scratch::new("collector");
    let object = compile_c(scratch.path(), PROGRAM);
    let image = krusty::native::link_program(&[&object], target)
        .unwrap_or_else(|error| panic!("the test program must link against the runtime: {error}"));
    let executable = scratch.path().join("program");
    std::fs::write(&executable, &image).expect("write executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    let output = common::run_freshly_written(Command::new(&executable).env_clear())
        .expect("run the built executable");
    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        0,
        "{}\nstderr: {}",
        describe(code),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut expected = (0..300)
        .map(|i| char::from(b'0' + (i % 10) as u8))
        .collect::<String>();
    expected.push_str("\nliteral\nkotlin.Unit\nnull\nmixed\n");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        expected,
        "a string built across collections must keep its text, and values outside the heap must \
         pass through a collection untouched"
    );
}

#[test]
fn the_runtime_ships_the_collector_with_every_program() {
    // A program gets the collector by linking against the prebuilt runtime, and the runtime is
    // three objects per architecture: values, heap, entry. This pins that the heap is among them
    // for every target this build carries, independent of whether a C compiler is present here.
    let mut carried = 0;
    for &target in NativeTarget::ALL {
        let Some(objects) = krusty::native::runtime_objects(target) else {
            continue;
        };
        carried += 1;
        let names = objects.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        for required in ["krusty_rt", "krusty_gc", "krusty_start"] {
            assert!(
                names.iter().any(|name| name.contains(required)),
                "the prebuilt runtime for {target} must carry {required}: {names:?}"
            );
        }
    }
    if carried == 0 {
        eprintln!("skipping: this build of krusty carries no prebuilt runtime");
    }
}
