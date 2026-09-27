/* Collections and elements a PROGRAM defines, for drivers that check how the runtime walks what it
   did not make: every call the runtime makes into them is logged, and any of them can be told to
   throw.

   `Tag` is an element whose `equals`, `hashCode` and `toString` are its own. `Seq` is a class of
   the program's that implements `Iterable` (or `List`, when its descriptor says so) over up to
   eight elements, reached the way the runtime reaches any program collection: through the
   `iterator`, `hasNext` and `next` thunks its descriptor records. A thrown exception is fresh each
   time and remembered in `thrown`, so a driver checks the pending exception by identity. Include
   this, and not `collections_later_tiers.h` beside it, from exactly one file per driver. */
#ifndef KRUSTY_TEST_PROGRAM_COLLECTIONS_H
#define KRUSTY_TEST_PROGRAM_COLLECTIONS_H

#include "collections_later_tiers.h"

/* ---- the call log ---------------------------------------------------------------------------- */

static char call_log[512];
static kt_int call_log_length;
/* The exception the last throwing call threw; a collector root once `program_begin` has run. */
static KRef thrown;

static void log_text(const char *text) {
    for (kt_int at = 0; text[at] != 0; at++) {
        if (call_log_length == (kt_int)sizeof(call_log)) {
            KT_SYS_FAIL("the call log overflowed\n");
        }
        call_log[call_log_length++] = text[at];
    }
}

static void log_int(kt_int value) {
    char digits[12];
    kt_int count = 0;
    do {
        digits[count++] = (char)('0' + value % 10);
        value /= 10;
    } while (value > 0);
    while (count > 0) {
        char one[2] = {digits[--count], 0};
        log_text(one);
    }
}

/* Whether the log is exactly `expected`; it is cleared either way. */
static kt_boolean log_is(const char *expected) {
    kt_int length = 0;
    while (expected[length] != 0) {
        length++;
    }
    kt_boolean same = length == call_log_length;
    for (kt_int at = 0; same && at < length; at++) {
        same = call_log[at] == expected[at];
    }
    call_log_length = 0;
    return same;
}

static void throw_fresh(void) {
    thrown = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
    kt_throw(thrown);
}

/* The exception pending is exactly the last one thrown; the slot is cleared. */
static kt_boolean threw_last(void) {
    kt_boolean same = thrown != NULL && kt_pending_exception() == thrown;
    kt_clear_pending();
    thrown = NULL;
    return same;
}

/* ---- Tag: an element with members of its own ------------------------------------------------ */

typedef struct Tag {
    KObjectHeader header;
    kt_int n;
    /* Which members throw: 'e' equals, 'h' hashCode, 's' toString. */
    const char *throws;
} Tag;

static const KType tag_type;

static kt_boolean tag_throws(KRef self, char member) {
    for (const char *at = ((const Tag *)self)->throws; *at != 0; at++) {
        if (*at == member) {
            throw_fresh();
            return 1;
        }
    }
    return 0;
}

static kt_boolean tag_equals(KRef self, KRef other) {
    log_text("eq(");
    log_int(((const Tag *)self)->n);
    log_text(") ");
    if (tag_throws(self, 'e')) {
        return 1;
    }
    return other != NULL && type_of(other) == &tag_type &&
           ((const Tag *)other)->n == ((const Tag *)self)->n;
}

static kt_int tag_hash_code(KRef self) {
    log_text("hash(");
    log_int(((const Tag *)self)->n);
    log_text(") ");
    if (tag_throws(self, 'h')) {
        return 7;
    }
    return ((const Tag *)self)->n;
}

/* `kt_string_utf8` names its bytes without copying, so each rendering is a literal. */
static KRef tag_to_string(KRef self) {
    static const char *const names[] = {"T0", "T1", "T2", "T3", "T4", "T5", "T6", "T7"};
    kt_int n = ((const Tag *)self)->n;
    log_text("str(");
    log_int(n);
    log_text(") ");
    if (tag_throws(self, 's')) {
        return NULL;
    }
    if (n < 0 || n > 7) {
        KT_SYS_FAIL("a tag this driver does not render was rendered\n");
    }
    return kt_string_utf8(names[n], 2);
}

static const kt_fn tag_vtable[] = {(kt_fn)tag_equals, (kt_fn)tag_hash_code, (kt_fn)tag_to_string};

static const KType tag_type = {
    .name = "Tag",
    .name_length = sizeof("Tag") - 1,
    .instance_size = sizeof(Tag),
    .super = &kt_type_any,
    .vtable = tag_vtable,
    .vtable_length = 3,
};

static KRef tag(kt_int n, const char *throwing) {
    Tag *made = (Tag *)kt_gc_allocate(&tag_type, sizeof(Tag));
    made->n = n;
    made->throws = throwing;
    return (KRef)made;
}

/* ---- Seq: a collection of the program's own ------------------------------------------------- */

typedef struct Seq {
    KObjectHeader header;
    KRef elements[8];
    kt_int size;
    /* Which calls throw: 'i' iterator(), and 'h'/'n' the iterator's hasNext()/next() once its
       cursor reaches `throw_at`. A throwing `hasNext` answers `placeholder`, a throwing `next` and
       `iterator` answer NULL or a live object as `placeholder` says, so a driver can check both
       polarities of what a caller might take for an answer. */
    const char *throws;
    kt_int throw_at;
    kt_boolean placeholder;
} Seq;

typedef struct SeqIterator {
    KObjectHeader header;
    KRef seq;
    kt_int at;
} SeqIterator;

static const uint32_t seq_offsets[] = {offsetof(Seq, elements[0]), offsetof(Seq, elements[1]),
                                       offsetof(Seq, elements[2]), offsetof(Seq, elements[3]),
                                       offsetof(Seq, elements[4]), offsetof(Seq, elements[5]),
                                       offsetof(Seq, elements[6]), offsetof(Seq, elements[7])};
static const uint32_t seq_iterator_offsets[] = {offsetof(SeqIterator, seq)};

static kt_boolean seq_throws(const Seq *seq, char call, kt_int at) {
    if (at != seq->throw_at) {
        return 0;
    }
    for (const char *c = seq->throws; *c != 0; c++) {
        if (*c == call) {
            throw_fresh();
            return 1;
        }
    }
    return 0;
}

static const KType seq_iterator_type;

static KRef seq_iterator(KRef self) {
    Seq *seq = (Seq *)self;
    log_text("iterator ");
    if (seq_throws(seq, 'i', seq->throw_at)) {
        return seq->placeholder ? self : NULL;
    }
    SeqIterator *iterator = (SeqIterator *)kt_gc_allocate(&seq_iterator_type, sizeof(SeqIterator));
    iterator->seq = self;
    iterator->at = 0;
    return (KRef)iterator;
}

static kt_boolean seq_has_next(KRef self) {
    SeqIterator *iterator = (SeqIterator *)self;
    const Seq *seq = (const Seq *)iterator->seq;
    log_text("hasNext ");
    if (seq_throws(seq, 'h', iterator->at)) {
        return seq->placeholder;
    }
    return iterator->at < seq->size;
}

static KRef seq_next(KRef self) {
    SeqIterator *iterator = (SeqIterator *)self;
    const Seq *seq = (const Seq *)iterator->seq;
    log_text("next ");
    if (seq_throws(seq, 'n', iterator->at)) {
        return seq->placeholder ? seq->elements[0] : NULL;
    }
    if (iterator->at >= seq->size) {
        KT_SYS_FAIL("the runtime asked next() past the end without asking hasNext()\n");
    }
    return seq->elements[iterator->at++];
}

static const KType seq_iterator_type = {
    .name = "SeqIterator",
    .name_length = sizeof("SeqIterator") - 1,
    .instance_size = sizeof(SeqIterator),
    .reference_count = 1,
    .reference_offsets = seq_iterator_offsets,
    .super = &kt_type_any,
    .walk_has_next = seq_has_next,
    .walk_next = seq_next,
};

/* An `Iterable` of the program's, and a `List` of the program's: the same walk, and only the
   interfaces the descriptor names differ. */
static const KType *const seq_iterable_interfaces[] = {&kt_type_iterable_interface};
static const KType *const seq_list_interfaces[] = {
    &kt_type_list_interface, &kt_type_collection_interface, &kt_type_iterable_interface};

static const KType seq_type = {
    .name = "Seq",
    .name_length = sizeof("Seq") - 1,
    .instance_size = sizeof(Seq),
    .reference_count = 8,
    .reference_offsets = seq_offsets,
    .super = &kt_type_any,
    .interfaces = seq_iterable_interfaces,
    .interface_count = 1,
    .walk_iterator = seq_iterator,
};

static const KType seq_list_type = {
    .name = "SeqList",
    .name_length = sizeof("SeqList") - 1,
    .instance_size = sizeof(Seq),
    .reference_count = 8,
    .reference_offsets = seq_offsets,
    .super = &kt_type_any,
    .interfaces = seq_list_interfaces,
    .interface_count = 3,
    .walk_iterator = seq_iterator,
};

/* A program collection over `count` elements (at most eight) of `type`, with nothing that throws
   until `seq_throwing` says so. */
static KRef seq_of(const KType *type, KRef *elements, kt_int count) {
    Seq *seq = (Seq *)kt_gc_allocate(type, sizeof(Seq));
    for (kt_int at = 0; at < count; at++) {
        seq->elements[at] = elements[at];
    }
    seq->size = count;
    seq->throws = "";
    seq->throw_at = -1;
    seq->placeholder = 0;
    return (KRef)seq;
}

static KRef seq_throwing(KRef seq, const char *calls, kt_int at, kt_boolean placeholder) {
    ((Seq *)seq)->throws = calls;
    ((Seq *)seq)->throw_at = at;
    ((Seq *)seq)->placeholder = placeholder;
    return seq;
}

/* Record the stack bottom, and keep `thrown` a root. */
#define PROGRAM_BEGIN()                                                                            \
    DRIVER_BEGIN();                                                                                \
    kt_gc_add_global_root((void **)&thrown)

#endif
