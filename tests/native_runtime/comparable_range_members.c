/* `ComparableRange`'s `equals`, `hashCode` and `toString` over a program's own `Comparable`,
   checked by the exact sequence of calls they make into the program and by where they stop when
   one of those calls throws. Kotlin's members are

       equals:   other is ComparableRange<*> && (isEmpty() && other.isEmpty() ||
                     start == other.start && endInclusive == other.endInclusive)
       hashCode: if (isEmpty()) -1 else 31 * start.hashCode() + endInclusive.hashCode()
       toString: "$start..$endInclusive"

   so `other.isEmpty()` — a call into `other`'s `compareTo` — happens only when `self` is empty, and
   the first call that throws is the last call made. `equals` used to ask `other.isEmpty()`
   unconditionally, and `hashCode` folded the second bound's hash without looking for the exception
   it may have left pending.

   The driver prints each member's answer, or the message of the exception it stopped at, with the
   calls it made, and the harness compares the lines with what `comparable_range_members.kt`
   answers under the reference kotlinc, where `V` is the program class this driver stands in for.
   The driver itself checks that the exception left pending is the very one the member threw, and
   the exact calls again. */
#include "transcript.h"

typedef struct V {
    KObjectHeader header;
    kt_int n;
    const char *tag;
    /* Which members throw: 'c' compareTo, 'e' equals, 'h' hashCode, 's' toString. */
    const char *throws;
    /* What this object's members threw last, so the pending exception is checked by identity. */
    KRef thrown;
} V;

/* The calls made into the program since the log was last cleared, as the Kotlin program logs
   them. */
static char log_text[256];
static kt_int log_length;

static void log_append(const char *text) {
    for (kt_int at = 0; text[at] != 0; at++) {
        if (log_length == (kt_int)sizeof(log_text)) {
            KT_SYS_FAIL("the call log overflowed\n");
        }
        log_text[log_length++] = text[at];
    }
}

static void log_call(const char *member, KRef self, KRef other) {
    log_append(member);
    log_append("(");
    log_append(((const V *)self)->tag);
    if (other != NULL) {
        log_append(",");
        log_append(((const V *)other)->tag);
    }
    log_append(") ");
}

static kt_boolean log_is(const char *expected) {
    kt_int length = 0;
    while (expected[length] != 0) {
        length++;
    }
    kt_boolean same = length == log_length;
    for (kt_int at = 0; same && at < length; at++) {
        same = log_text[at] == expected[at];
    }
    log_length = 0;
    return same;
}

static kt_int length_of(const char *text) {
    kt_int length = 0;
    while (text[length] != 0) {
        length++;
    }
    return length;
}

/* Whether `self`'s member `member` throws, and if so, throws with the message `<prefix><tag>`, as
   the program's `Boom("cmp:$tag")` does. */
static kt_boolean throws(KRef self, char member, const char *prefix) {
    V *v = (V *)self;
    for (kt_int at = 0; v->throws[at] != 0; at++) {
        if (v->throws[at] == member) {
            KRef message = kt_string_plus(kt_string_utf8(prefix, length_of(prefix)),
                                          kt_string_utf8(v->tag, length_of(v->tag)));
            v->thrown = kt_throwable_new(&kt_type_illegal_state_exception, message);
            kt_throw(v->thrown);
            return true;
        }
    }
    return false;
}

static kt_int v_compare(KRef self, KRef other) {
    log_call("cmp", self, other);
    if (throws(self, 'c', "cmp:")) {
        return 0;
    }
    kt_int a = ((const V *)self)->n;
    kt_int b = ((const V *)other)->n;
    return a < b ? -1 : a > b ? 1 : 0;
}

static const KType v_type;

static kt_boolean v_equals(KRef self, KRef other) {
    log_call("eq", self, NULL);
    if (throws(self, 'e', "eq:")) {
        return true;
    }
    return other != NULL && type_of(other) == &v_type &&
           ((const V *)other)->n == ((const V *)self)->n;
}

static kt_int v_hash_code(KRef self) {
    log_call("hash", self, NULL);
    if (throws(self, 'h', "hash:")) {
        return 0;
    }
    return ((const V *)self)->n;
}

/* `kt_string_utf8` names its bytes without copying, so each rendering is a literal. */
static KRef v_to_string(KRef self) {
    log_call("str", self, NULL);
    if (throws(self, 's', "str:")) {
        return NULL;
    }
    switch (((const V *)self)->n) {
    case 1:
        return kt_string_utf8("V1", 2);
    case 2:
        return kt_string_utf8("V2", 2);
    case 3:
        return kt_string_utf8("V3", 2);
    case 5:
        return kt_string_utf8("V5", 2);
    default:
        KT_SYS_FAIL("a V this driver does not render was rendered\n");
        return NULL;
    }
}

static const kt_fn v_vtable[] = {(kt_fn)v_equals, (kt_fn)v_hash_code, (kt_fn)v_to_string};

static const KType v_type = {
    .name = "V",
    .name_length = sizeof("V") - 1,
    .instance_size = sizeof(V),
    .reference_count = 1,
    .reference_offsets = (const uint32_t[]){offsetof(V, thrown)},
    .super = &kt_type_any,
    .vtable = v_vtable,
    .vtable_length = 3,
};

static KRef v(kt_int n, const char *tag, const char *throwing) {
    V *made = (V *)kt_gc_allocate(&v_type, sizeof(V));
    made->n = n;
    made->tag = tag;
    made->throws = throwing;
    return (KRef)made;
}

static KRef range(KRef start, KRef end) { return kt_comparable_range(start, end, v_compare); }

/* The line for a member's answer: `label = answer | calls`, the calls being the log so far. */
static void say_calls(void) {
    say(" | ");
    say_bytes(log_text, log_length);
    say("\n");
}

static void show_bool(const char *label, kt_boolean answer) {
    say(label);
    say(" = ");
    say_bool(answer);
    say_calls();
}

static void show_int(const char *label, kt_int answer) {
    say(label);
    say(" = ");
    say_long(answer);
    say_calls();
}

static void show_text(const char *label, KRef answer) {
    CHECK(kt_pending_exception() == NULL, "an ordinary member raised\n");
    say(label);
    say(" = ");
    say_text(answer);
    say_calls();
}

/* A member that threw: the exception pending is the one `thrower` threw, and the line says what it
   said; the call log is exactly `calls`, and the answer returned beside the exception is not looked
   at. */
static void show_threw(const char *label, KRef thrower, const char *calls) {
    CHECK(kt_pending_exception() != NULL && kt_pending_exception() == ((const V *)thrower)->thrown,
          "the pending exception is not the throwing member's\n");
    say(label);
    say(" = threw ");
    say_text(kt_throwable_message(kt_pending_exception()));
    say_calls();
    CHECK(log_is(calls), "the calls made before the throw\n");
    kt_clear_pending();
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef a = range(v(1, "a", ""), v(3, "b", ""));
    KRef same = range(v(1, "c", ""), v(3, "d", ""));
    KRef empty1 = range(v(5, "e", ""), v(2, "f", ""));
    KRef empty2 = range(v(9, "g", ""), v(0, "h", ""));
    KRef other = range(v(1, "i", ""), v(4, "j", ""));

    show_bool("a==same", kt_equals(a, same));
    CHECK(log_is("cmp(a,b) eq(a) eq(b) "), "a == same\n");
    show_bool("a==empty1", kt_equals(a, empty1));
    CHECK(log_is("cmp(a,b) eq(a) "), "a == empty1\n");
    show_bool("empty1==empty2", kt_equals(empty1, empty2));
    CHECK(log_is("cmp(e,f) cmp(g,h) "), "empty1 == empty2\n");
    show_bool("empty1==a", kt_equals(empty1, a));
    CHECK(log_is("cmp(e,f) cmp(a,b) eq(e) "), "empty1 == a\n");
    show_bool("a==other", kt_equals(a, other));
    CHECK(log_is("cmp(a,b) eq(a) eq(b) "), "a == other\n");
    show_int("a.hash", kt_hash_code(a));
    CHECK(log_is("cmp(a,b) hash(a) hash(b) "), "a.hashCode()\n");
    show_int("empty1.hash", kt_hash_code(empty1));
    CHECK(log_is("cmp(e,f) "), "empty1.hashCode()\n");
    show_text("a.str", kt_to_string(a));
    CHECK(log_is("str(a) str(b) "), "a.toString()\n");
    show_text("empty1.str", kt_to_string(empty1));
    CHECK(log_is("str(e) str(f) "), "empty1.toString()\n");
    CHECK(kt_pending_exception() == NULL, "an ordinary member raised\n");

    KRef p = v(1, "p", "c");
    (void)kt_equals(range(p, v(3, "q", "")), a);
    show_threw("cmpThrows==a", p, "cmp(p,q) ");

    KRef r = v(1, "r", "c");
    (void)kt_equals(empty1, range(r, v(0, "s", "")));
    show_threw("empty1==emptyOtherThrows", r, "cmp(e,f) cmp(r,s) ");

    KRef t = v(1, "t", "e");
    (void)kt_equals(range(t, v(3, "u", "")), same);
    show_threw("eqThrows==same", t, "cmp(t,u) eq(t) ");

    KRef w = v(3, "w", "e");
    (void)kt_equals(range(v(1, "v", ""), w), same);
    show_threw("endEqThrows==same", w, "cmp(v,w) eq(v) eq(w) ");

    KRef l = v(3, "l", "h");
    (void)kt_hash_code(range(v(1, "k", ""), l));
    show_threw("endHashThrows.hash", l, "cmp(k,l) hash(k) hash(l) ");

    KRef m = v(1, "m", "h");
    (void)kt_hash_code(range(m, v(3, "n", "h")));
    show_threw("startHashThrows.hash", m, "cmp(m,n) hash(m) ");

    KRef o = v(1, "o", "s");
    (void)kt_to_string(range(o, v(3, "y", "s")));
    show_threw("startStrThrows.str", o, "str(o) ");

    KRef zz = v(3, "zz", "s");
    (void)kt_to_string(range(v(1, "z", ""), zz));
    show_threw("endStrThrows.str", zz, "str(z) str(zz) ");

    kt_sys_write(1, "OK\n", 3);
}
