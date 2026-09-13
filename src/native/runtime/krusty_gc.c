/* krusty native runtime — generated; do not edit. */
#include "krusty_rt.h"
#include "krusty_sys.h"

/* ---- heap geometry --------------------------------------------------------------------------- */

#define KT_PAGE_BYTES 4096u
/* A chunk of small objects; objects above KT_LARGE_THRESHOLD get a chunk of their own. */
#define KT_CHUNK_BYTES (64u * 1024u)
#define KT_LARGE_THRESHOLD 2048u
/* The least allocation volume between collections; see kt_gc_collect for how it grows. */
#define KT_MIN_COLLECT_BYTES (4u * 1024u * 1024u)

static const uint32_t kt_size_classes[] = {16,  32,  48,  64,   96,   128,  192,
                                           256, 384, 512, 768, 1024, 1536, 2048};
#define KT_CLASS_COUNT ((uint32_t)(sizeof kt_size_classes / sizeof kt_size_classes[0]))

typedef struct KChunk {
    struct KChunk *next_in_class;
    struct KChunk *next_in_heap;
    uint8_t *objects;
    uint64_t *allocated;
    uint64_t *marked;
    /* Reuse list threaded through the free objects themselves. */
    void *free_list;
    size_t mapped_bytes;
    uint32_t object_size;
    uint32_t object_count;
    /* Objects never yet handed out; cheaper than seeding the free list at chunk creation. */
    uint32_t high_water;
    uint32_t live;
    /* A chunk holding one large object. Unmapped, not kept, once that object dies. */
    bool large;
} KChunk;

static KChunk *kt_class_chunks[KT_CLASS_COUNT];
static KChunk *kt_all_chunks;

/* Chunks sorted by address, so a candidate root resolves by binary search. Kept in raw mapped
   memory rather than the collected heap, for the obvious reason. */
static KChunk **kt_sorted;
static size_t kt_sorted_count;
static size_t kt_sorted_capacity;

static size_t kt_heap_bytes;
static size_t kt_allocated_since_collect;
static size_t kt_next_collect_at = KT_MIN_COLLECT_BYTES;

/* ---- chunk registry -------------------------------------------------------------------------- */

static void kt_register_chunk(KChunk *chunk) {
    if (kt_sorted_count == kt_sorted_capacity) {
        size_t capacity = kt_sorted_capacity == 0 ? 256 : kt_sorted_capacity * 2;
        KChunk **grown = (KChunk **)kt_map(capacity * sizeof(KChunk *));
        for (size_t i = 0; i < kt_sorted_count; i++) {
            grown[i] = kt_sorted[i];
        }
        if (kt_sorted != NULL) {
            kt_unmap(kt_sorted, kt_sorted_capacity * sizeof(KChunk *));
        }
        kt_sorted = grown;
        kt_sorted_capacity = capacity;
    }
    size_t at = kt_sorted_count;
    while (at > 0 && (uintptr_t)kt_sorted[at - 1] > (uintptr_t)chunk) {
        kt_sorted[at] = kt_sorted[at - 1];
        at--;
    }
    kt_sorted[at] = chunk;
    kt_sorted_count++;
}

static void kt_unregister_chunk(KChunk *chunk) {
    size_t at = 0;
    while (at < kt_sorted_count && kt_sorted[at] != chunk) {
        at++;
    }
    for (; at + 1 < kt_sorted_count; at++) {
        kt_sorted[at] = kt_sorted[at + 1];
    }
    kt_sorted_count--;
}

/* The chunk whose object area contains `address`, or NULL. Anything outside the heap — static
   storage, the stack, an integer that looks like nothing — resolves to NULL here, which is the one
   place that decides an address is not an object. */
static KChunk *kt_chunk_of(uintptr_t address) {
    size_t low = 0;
    size_t high = kt_sorted_count;
    while (low < high) {
        size_t mid = low + (high - low) / 2;
        KChunk *chunk = kt_sorted[mid];
        uintptr_t start = (uintptr_t)chunk->objects;
        uintptr_t end = start + (uintptr_t)chunk->object_size * chunk->object_count;
        if (address < start) {
            high = mid;
        } else if (address >= end) {
            low = mid + 1;
        } else {
            return chunk;
        }
    }
    return NULL;
}

static uint32_t kt_class_for(uint32_t size) {
    for (uint32_t i = 0; i < KT_CLASS_COUNT; i++) {
        if (size <= kt_size_classes[i]) {
            return i;
        }
    }
    return KT_CLASS_COUNT;
}

static KChunk *kt_new_chunk(uint32_t object_size, uint32_t object_count, bool large) {
    size_t bitmap_words = ((size_t)object_count + 63u) / 64u;
    size_t header = sizeof(KChunk) + 2 * bitmap_words * sizeof(uint64_t);
    header = (header + 15u) & ~(size_t)15u;
    size_t total = header + (size_t)object_size * object_count;
    total = (total + KT_PAGE_BYTES - 1) & ~(size_t)(KT_PAGE_BYTES - 1);

    /* kt_map returns zero-filled pages, so the bitmaps start clear and the header fields start
       at zero without a pass over them. */
    KChunk *chunk = (KChunk *)kt_map(total);
    chunk->allocated = (uint64_t *)((uint8_t *)chunk + sizeof(KChunk));
    chunk->marked = chunk->allocated + bitmap_words;
    chunk->objects = (uint8_t *)chunk + header;
    chunk->object_size = object_size;
    /* Page rounding may leave room past `object_count` objects; the bitmaps were sized for
       exactly that many, so the slack stays unused rather than unaccounted for. */
    chunk->object_count = object_count;
    chunk->mapped_bytes = total;
    chunk->large = large;
    chunk->next_in_heap = kt_all_chunks;
    kt_all_chunks = chunk;
    kt_heap_bytes += total;
    kt_register_chunk(chunk);
    return chunk;
}

static bool kt_bit(const uint64_t *bits, uint32_t index) {
    return (bits[index / 64u] >> (index % 64u)) & 1u;
}

static void kt_set_bit(uint64_t *bits, uint32_t index) {
    bits[index / 64u] |= (uint64_t)1 << (index % 64u);
}

static void kt_clear_bit(uint64_t *bits, uint32_t index) {
    bits[index / 64u] &= ~((uint64_t)1 << (index % 64u));
}

/* ---- marking --------------------------------------------------------------------------------- */

/* An explicit mark stack: the depth of a Kotlin object graph is program data, and recursing on it
   would turn a deep list into a stack overflow inside the collector. */
static void **kt_mark_stack;
static size_t kt_mark_top;
static size_t kt_mark_capacity;

static void kt_mark_push(void *object) {
    if (kt_mark_top == kt_mark_capacity) {
        size_t capacity = kt_mark_capacity == 0 ? 1024 : kt_mark_capacity * 2;
        void **grown = (void **)kt_map(capacity * sizeof(void *));
        for (size_t i = 0; i < kt_mark_top; i++) {
            grown[i] = kt_mark_stack[i];
        }
        if (kt_mark_stack != NULL) {
            kt_unmap(kt_mark_stack, kt_mark_capacity * sizeof(void *));
        }
        kt_mark_stack = grown;
        kt_mark_capacity = capacity;
    }
    kt_mark_stack[kt_mark_top++] = object;
}

/* Mark whatever `address` points at, if it points into an allocated object. Interior pointers
   count: a C compiler may keep a pointer to a field rather than to the object. A freed slot is
   never marked, so a stale pointer to one cannot resurrect it — its first word is a free-list
   link, not a type. */
static void kt_mark_candidate(uintptr_t address) {
    KChunk *chunk = kt_chunk_of(address);
    if (chunk == NULL) {
        return;
    }
    uint32_t index = (uint32_t)((address - (uintptr_t)chunk->objects) / chunk->object_size);
    if (!kt_bit(chunk->allocated, index) || kt_bit(chunk->marked, index)) {
        return;
    }
    kt_set_bit(chunk->marked, index);
    kt_mark_push(chunk->objects + (size_t)index * chunk->object_size);
}

/* Drain the mark stack, tracing each object's reference fields PRECISELY from its type. */
static void kt_trace(void) {
    while (kt_mark_top > 0) {
        uint8_t *object = (uint8_t *)kt_mark_stack[--kt_mark_top];
        const KType *type = ((KObjectHeader *)object)->type;
        if (type == NULL) {
            continue;
        }
        for (uint32_t i = 0; i < type->reference_count; i++) {
            void *field = *(void **)(object + type->reference_offsets[i]);
            if (field != NULL) {
                kt_mark_candidate((uintptr_t)field);
            }
        }
    }
}

/* ---- roots ----------------------------------------------------------------------------------- */

/* Where the program's stack began, recorded by kt_runtime_init. */
static uintptr_t kt_stack_bottom;

/* Global reference slots the emitted program declares (top-level properties). Registered rather
   than discovered, because a freestanding program has no portable way to find its own data
   section, and guessing at one would either miss roots or scan unrelated memory. */
#define KT_MAX_GLOBALS 4096
static void **kt_globals[KT_MAX_GLOBALS];
static uint32_t kt_global_count;

void kt_gc_add_global_root(void **slot) {
    if (kt_global_count == KT_MAX_GLOBALS) {
        KT_SYS_FAIL("krusty: too many global roots\n");
    }
    kt_globals[kt_global_count++] = slot;
}

void kt_runtime_init(void *stack_bottom) { kt_stack_bottom = (uintptr_t)stack_bottom; }

/* Scan a stack range word by word. A reference is stored aligned, so stepping by pointer size is
   sufficient. */
static void kt_scan_range(uintptr_t low, uintptr_t high) {
    low = (low + sizeof(void *) - 1) & ~(uintptr_t)(sizeof(void *) - 1);
    for (uintptr_t at = low; at + sizeof(void *) <= high; at += sizeof(void *)) {
        kt_mark_candidate(*(uintptr_t *)at);
    }
}

/* Spill the callee-saved registers to the stack so the scan below sees them. A reference whose
   only copy is in a register is invisible to a stack scan, and would be collected while live.
   Not inlined: the scan starts at this frame's `saved`, which must be the deepest frame with
   anything to find. */
__attribute__((noinline)) static void kt_scan_registers_and_stack(void) {
    uintptr_t saved[16];
    for (unsigned i = 0; i < 16; i++) {
        saved[i] = 0;
    }
#if defined(__x86_64__)
    __asm__ volatile("movq %%rbx, %0\n\tmovq %%rbp, %1\n\tmovq %%r12, %2\n\tmovq %%r13, %3\n\t"
                     "movq %%r14, %4\n\tmovq %%r15, %5"
                     : "=m"(saved[0]), "=m"(saved[1]), "=m"(saved[2]), "=m"(saved[3]),
                       "=m"(saved[4]), "=m"(saved[5])
                     :
                     : "memory");
#elif defined(__aarch64__)
    __asm__ volatile("str x19, %0\n\tstr x20, %1\n\tstr x21, %2\n\tstr x22, %3\n\tstr x23, %4\n\t"
                     "str x24, %5\n\tstr x25, %6\n\tstr x26, %7\n\tstr x27, %8\n\tstr x28, %9"
                     : "=m"(saved[0]), "=m"(saved[1]), "=m"(saved[2]), "=m"(saved[3]),
                       "=m"(saved[4]), "=m"(saved[5]), "=m"(saved[6]), "=m"(saved[7]),
                       "=m"(saved[8]), "=m"(saved[9])
                     :
                     : "memory");
#elif defined(__riscv) && __riscv_xlen == 64
    __asm__ volatile("sd s0, %0\n\tsd s1, %1\n\tsd s2, %2\n\tsd s3, %3\n\tsd s4, %4\n\t"
                     "sd s5, %5\n\tsd s6, %6\n\tsd s7, %7\n\tsd s8, %8\n\tsd s9, %9\n\t"
                     "sd s10, %10\n\tsd s11, %11"
                     : "=m"(saved[0]), "=m"(saved[1]), "=m"(saved[2]), "=m"(saved[3]),
                       "=m"(saved[4]), "=m"(saved[5]), "=m"(saved[6]), "=m"(saved[7]),
                       "=m"(saved[8]), "=m"(saved[9]), "=m"(saved[10]), "=m"(saved[11])
                     :
                     : "memory");
#else
#error "krusty native: unsupported architecture"
#endif
    for (unsigned i = 0; i < 16; i++) {
        kt_mark_candidate(saved[i]);
    }

    /* Every frame between this one and the program's entry lies above `saved`. */
    kt_scan_range((uintptr_t)&saved[0], kt_stack_bottom);
}

/* ---- collection ------------------------------------------------------------------------------ */

/* Returns the bytes held by surviving objects. Dead small objects go on their chunk's reuse list;
   a dead large object's chunk is returned to the kernel. */
static size_t kt_sweep(void) {
    size_t live_bytes = 0;
    KChunk **link = &kt_all_chunks;
    while (*link != NULL) {
        KChunk *chunk = *link;
        chunk->live = 0;
        for (uint32_t index = 0; index < chunk->high_water; index++) {
            if (!kt_bit(chunk->allocated, index)) {
                continue;
            }
            if (kt_bit(chunk->marked, index)) {
                kt_clear_bit(chunk->marked, index);
                chunk->live++;
                continue;
            }
            kt_clear_bit(chunk->allocated, index);
            void *object = chunk->objects + (size_t)index * chunk->object_size;
            /* Thread the reuse list through the dead object itself. */
            *(void **)object = chunk->free_list;
            chunk->free_list = object;
        }
        live_bytes += (size_t)chunk->live * chunk->object_size;
        if (chunk->large && chunk->live == 0) {
            *link = chunk->next_in_heap;
            kt_unregister_chunk(chunk);
            kt_heap_bytes -= chunk->mapped_bytes;
            kt_unmap(chunk, chunk->mapped_bytes);
            continue;
        }
        link = &chunk->next_in_heap;
    }
    return live_bytes;
}

static bool kt_collecting;

void kt_gc_collect(void) {
    /* Without the stack bottom there are no roots, and "no roots" would free everything the
       program holds. That is a bug in whoever produced the entry point, and it must not look
       like a subtle memory corruption. */
    if (kt_stack_bottom == 0) {
        KT_SYS_FAIL("krusty: collection before kt_runtime_init recorded the stack\n");
    }
    if (kt_collecting) {
        return;
    }
    kt_collecting = true;
    for (uint32_t i = 0; i < kt_global_count; i++) {
        void *value = *kt_globals[i];
        if (value != NULL) {
            kt_mark_candidate((uintptr_t)value);
        }
    }
    kt_scan_registers_and_stack();
    kt_trace();
    size_t live_bytes = kt_sweep();
    /* The next collection comes after as many bytes again as survived this one (at least the
       minimum), so the heap settles at about twice the live set rather than collecting on every
       few allocations when the live set is large. */
    kt_allocated_since_collect = 0;
    kt_next_collect_at = live_bytes > KT_MIN_COLLECT_BYTES ? live_bytes : KT_MIN_COLLECT_BYTES;
    kt_collecting = false;
}

/* ---- allocation ------------------------------------------------------------------------------ */

static void *kt_take_from(KChunk *chunk) {
    if (chunk->free_list != NULL) {
        void *object = chunk->free_list;
        chunk->free_list = *(void **)object;
        uint32_t index = (uint32_t)(((uint8_t *)object - chunk->objects) / chunk->object_size);
        kt_set_bit(chunk->allocated, index);
        chunk->live++;
        return object;
    }
    if (chunk->high_water < chunk->object_count) {
        uint32_t index = chunk->high_water++;
        kt_set_bit(chunk->allocated, index);
        chunk->live++;
        return chunk->objects + (size_t)index * chunk->object_size;
    }
    return NULL;
}

void *kt_gc_allocate(const KType *type, uint32_t size) {
    /* Room for the header, which also covers the reuse-list link a swept object holds. */
    if (size < sizeof(KObjectHeader)) {
        size = sizeof(KObjectHeader);
    }

    if (kt_allocated_since_collect >= kt_next_collect_at) {
        kt_gc_collect();
    }

    uint8_t *object = NULL;
    uint32_t object_size;
    uint32_t class_index = kt_class_for(size);
    if (class_index < KT_CLASS_COUNT) {
        object_size = kt_size_classes[class_index];
        for (KChunk *chunk = kt_class_chunks[class_index]; chunk != NULL;
             chunk = chunk->next_in_class) {
            object = (uint8_t *)kt_take_from(chunk);
            if (object != NULL) {
                break;
            }
        }
        if (object == NULL) {
            KChunk *chunk = kt_new_chunk(object_size, KT_CHUNK_BYTES / object_size, false);
            chunk->next_in_class = kt_class_chunks[class_index];
            kt_class_chunks[class_index] = chunk;
            object = (uint8_t *)kt_take_from(chunk);
        }
    } else {
        /* One chunk per large object, unmapped when it dies. */
        object_size = (size + 15u) & ~15u;
        KChunk *chunk = kt_new_chunk(object_size, 1, true);
        object = (uint8_t *)kt_take_from(chunk);
    }
    if (object == NULL) {
        kt_fail_oom();
    }
    kt_allocated_since_collect += object_size;

    /* Every slot size is a multiple of 16, so clearing by words covers it exactly. */
    uint64_t *words = (uint64_t *)object;
    for (uint32_t i = 0; i < object_size / sizeof(uint64_t); i++) {
        words[i] = 0;
    }
    ((KObjectHeader *)object)->type = type;
    return object;
}

/* ---- introspection, for tests ---------------------------------------------------------------- */

size_t kt_gc_live_objects(void) {
    size_t objects = 0;
    for (KChunk *chunk = kt_all_chunks; chunk != NULL; chunk = chunk->next_in_heap) {
        objects += chunk->live;
    }
    return objects;
}

size_t kt_gc_heap_bytes(void) { return kt_heap_bytes; }

size_t kt_gc_live_bytes(void) {
    size_t bytes = 0;
    for (KChunk *chunk = kt_all_chunks; chunk != NULL; chunk = chunk->next_in_heap) {
        bytes += (size_t)chunk->live * chunk->object_size;
    }
    return bytes;
}
