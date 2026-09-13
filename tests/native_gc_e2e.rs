//! The native runtime's object model, allocator and collector, exercised from C.
//!
//! The emitter cannot reach the collector's interesting cases yet — it declines classes, so no
//! Kotlin program can build a cycle or an interior pointer — but the runtime has to be right before
//! those land, not after. So this test links a hand-written C program against the emitted runtime
//! artifacts (the same `krusty_rt.c`, `krusty_gc.c` and `krusty_start.c` every Kotlin program
//! links) and RUNS it. The program supplies `kt_program_entry` itself, in place of the shim the
//! backend generates for a Kotlin `main`, and exits with a distinct nonzero code per failed
//! assertion so a failure names the property that broke.
//!
//! Nothing here inspects generated C. A collector that looks right and frees a live object is the
//! failure a text assertion cannot see.
//!
//! Skips rather than fails when no C compiler can build for this host.

use std::path::{Path, PathBuf};

use krusty::native::{NativeBackend, NativeTarget};

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

/// Whether this environment can build and run a native program at all.
fn available() -> bool {
    NativeTarget::host().is_some_and(krusty::native::can_build)
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
static const KType node_type = {"Node", 4, sizeof(Node), 1, node_references};

/* No reference fields at all: `address` holds a pointer's BITS in a kt_long, which a precise
   tracer must not follow. */
typedef struct Leaf {
    KObjectHeader header;
    kt_long value;
    kt_long address;
} Leaf;
static const KType leaf_type = {"Leaf", 4, sizeof(Leaf), 0, NULL};

/* Larger than the largest small size class, so it takes the large-object path. */
typedef struct Big {
    KObjectHeader header;
    kt_long values[512];
} Big;
static const KType big_type = {"Big", 3, sizeof(Big), 0, NULL};

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
    kt_exit(0);
}
"#;

#[test]
fn the_collector_reclaims_garbage_and_keeps_what_is_reachable() {
    if !available() {
        eprintln!("skipping: needs a C compiler that can build for this host");
        return;
    }
    // The program links against exactly what a Kotlin program links against; only the entry shim
    // is replaced, because the program IS the entry.
    let mut artifacts = NativeBackend::runtime_artifacts();
    artifacts.push(NativeBackend::start_artifact());
    artifacts.push(("gc_test.c".to_string(), PROGRAM.as_bytes().to_vec()));

    let scratch = Scratch::new("collector");
    let executable = scratch.path().join("program");
    krusty::native::link_executable(
        &artifacts,
        scratch.path(),
        &executable,
        NativeTarget::host().expect("checked by `available`"),
    )
    .unwrap_or_else(|error| panic!("the runtime and the test program must compile: {error}"));

    let output = std::process::Command::new(&executable)
        .output()
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
    expected.push_str("\nliteral\nkotlin.Unit\nnull\n");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        expected,
        "a string built across collections must keep its text, and values outside the heap must \
         pass through a collection untouched"
    );
}

#[test]
fn the_runtime_ships_the_collector_with_every_program() {
    // `link.rs` compiles every `.c` artifact, so a program gets the collector by the backend
    // emitting it — this pins that it does, independent of whether a C compiler is present.
    let names = NativeBackend::runtime_artifacts()
        .into_iter()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    for required in [
        krusty::native::SYS_HEADER,
        krusty::native::RUNTIME_HEADER,
        krusty::native::RUNTIME_SOURCE,
        krusty::native::GC_SOURCE,
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "the runtime must emit {required}: {names:?}"
        );
    }
    assert_eq!(
        NativeBackend::start_artifact().0,
        krusty::native::START_SOURCE
    );
}
