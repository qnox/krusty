/* Keys that write down every call a map makes into them, for the map and set drivers to compare
   with what Kotlin makes on the JVM.

   `Key` is the Kotlin class

       class K(val n: Int, val h: Int, val tag: String) {
           override fun equals(other: Any?): Boolean {
               log.append("eq($tag,${(other as? K)?.tag}) "); return other is K && other.n == n
           }
           override fun hashCode(): Int { log.append("hash($tag) "); return h }
           override fun toString() = tag
       }

   and `Asymmetric` is one whose `equals` accepts a `Key` that never accepts it back:

       class A(val n: Int, val tag: String) {
           override fun equals(other: Any?): Boolean {
               log.append("eqA($tag) "); return other is K && other.n == n
           }
           override fun hashCode(): Int { log.append("hashA($tag) "); return 7 }
           override fun toString() = tag
       }

   The Kotlin program beside each driver declares these classes, and the harness compares what the
   two answer. Include this from exactly one file per driver. */
#ifndef KRUSTY_TEST_LOGGED_KEYS_H
#define KRUSTY_TEST_LOGGED_KEYS_H

#include "collections_later_tiers.h"

typedef struct Key {
    KObjectHeader header;
    kt_int n;
    kt_int h;
    const char *tag;
} Key;

static char call_log[1024];
static kt_int call_log_length;

static void log_text(const char *text) {
    for (; *text != 0 && call_log_length < (kt_int)sizeof(call_log); text++) {
        call_log[call_log_length++] = *text;
    }
}

/* Whether the calls written down since the last check are exactly `expected`; starts afresh. */
static kt_boolean logged(const char *expected) {
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

static kt_boolean key_equals(KRef self, KRef other);
static kt_int key_hash_code(KRef self);
static KRef key_to_string(KRef self);
static kt_boolean asymmetric_equals(KRef self, KRef other);
static kt_int asymmetric_hash_code(KRef self);

static const kt_fn key_vtable[] = {(kt_fn)key_equals, (kt_fn)key_hash_code,
                                   (kt_fn)key_to_string};
static const kt_fn asymmetric_vtable[] = {(kt_fn)asymmetric_equals, (kt_fn)asymmetric_hash_code,
                                          (kt_fn)key_to_string};

static const KType key_type = {
    .name = "K",
    .name_length = 1,
    .instance_size = sizeof(Key),
    .super = &kt_type_any,
    .vtable = key_vtable,
    .vtable_length = 3,
};

static const KType asymmetric_type = {
    .name = "A",
    .name_length = 1,
    .instance_size = sizeof(Key),
    .super = &kt_type_any,
    .vtable = asymmetric_vtable,
    .vtable_length = 3,
};

static KRef key_of(const KType *type, kt_int n, kt_int h, const char *tag) {
    Key *key = (Key *)kt_gc_allocate(type, sizeof(Key));
    key->n = n;
    key->h = h;
    key->tag = tag;
    return (KRef)key;
}

/* `K(n, h, tag)` and `A(n, tag)`. */
static KRef key(kt_int n, kt_int h, const char *tag) { return key_of(&key_type, n, h, tag); }
static KRef asymmetric(kt_int n, const char *tag) { return key_of(&asymmetric_type, n, 7, tag); }

static const Key *as_key(KRef value) {
    return value != NULL && type_of(value) == &key_type ? (const Key *)value : NULL;
}

static kt_boolean key_equals(KRef self, KRef other) {
    log_text("eq(");
    log_text(((const Key *)self)->tag);
    log_text(",");
    log_text(as_key(other) != NULL ? as_key(other)->tag : "null");
    log_text(") ");
    return as_key(other) != NULL && as_key(other)->n == ((const Key *)self)->n;
}

static kt_int key_hash_code(KRef self) {
    log_text("hash(");
    log_text(((const Key *)self)->tag);
    log_text(") ");
    return ((const Key *)self)->h;
}

static KRef key_to_string(KRef self) {
    const char *tag = ((const Key *)self)->tag;
    kt_int length = 0;
    while (tag[length] != 0) {
        length++;
    }
    return kt_string_utf8(tag, length);
}

static kt_boolean asymmetric_equals(KRef self, KRef other) {
    log_text("eqA(");
    log_text(((const Key *)self)->tag);
    log_text(") ");
    return as_key(other) != NULL && as_key(other)->n == ((const Key *)self)->n;
}

static kt_int asymmetric_hash_code(KRef self) {
    log_text("hashA(");
    log_text(((const Key *)self)->tag);
    log_text(") ");
    return ((const Key *)self)->h;
}

#endif
