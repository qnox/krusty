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

   The expected calls and answers are Kotlin's, from this program compiled and run with the
   reference kotlinc 2.4.10 on the JVM (only the parts this driver checks are shown):

       val log = StringBuilder()
       class Boom(val tag: String) : RuntimeException(tag)
       class V(val n: Int, val tag: String, val throws: String = "") : Comparable<V> {
           override fun compareTo(other: V): Int {
               log.append("cmp($tag,${other.tag}) ")
               if ('c' in throws) throw Boom("cmp:$tag"); return n.compareTo(other.n)
           }
           override fun equals(other: Any?): Boolean {
               log.append("eq($tag) ")
               if ('e' in throws) throw Boom("eq:$tag"); return other is V && other.n == n
           }
           override fun hashCode(): Int {
               log.append("hash($tag) "); if ('h' in throws) throw Boom("hash:$tag"); return n
           }
           override fun toString(): String {
               log.append("str($tag) "); if ('s' in throws) throw Boom("str:$tag"); return "V$n"
           }
       }
       fun show(label: String, block: () -> Any?) {
           val r = try { block().toString() } catch (b: Boom) { "threw ${b.message}" }
           println("$label = $r | $log"); log.setLength(0)
       }
       fun main() {
           val a = V(1, "a")..V(3, "b"); val same = V(1, "c")..V(3, "d")
           val empty1 = V(5, "e")..V(2, "f"); val empty2 = V(9, "g")..V(0, "h")
           val other = V(1, "i")..V(4, "j")
           show("a==same") { a == same }              // true  | cmp(a,b) eq(a) eq(b)
           show("a==empty1") { a == empty1 }          // false | cmp(a,b) eq(a)
           show("empty1==empty2") { empty1 == empty2 } // true | cmp(e,f) cmp(g,h)
           show("empty1==a") { empty1 == a }          // false | cmp(e,f) cmp(a,b) eq(e)
           show("a==other") { a == other }            // false | cmp(a,b) eq(a) eq(b)
           show("a.hash") { a.hashCode() }            // 34 | cmp(a,b) hash(a) hash(b)
           show("empty1.hash") { empty1.hashCode() }  // -1 | cmp(e,f)
           show("a.str") { a.toString() }             // V1..V3 | str(a) str(b)
           show("empty1.str") { empty1.toString() }   // V5..V2 | str(e) str(f)
           val cmpThrows = V(1, "p", "c")..V(3, "q")
           show("cmpThrows==a") { cmpThrows == a }    // threw cmp:p | cmp(p,q)
           val emptyOtherThrows = V(1, "r", "c")..V(0, "s")
           show("empty1==emptyOtherThrows") { empty1 == emptyOtherThrows }
                                                      // threw cmp:r | cmp(e,f) cmp(r,s)
           val eqThrows = V(1, "t", "e")..V(3, "u")
           show("eqThrows==same") { eqThrows == same } // threw eq:t | cmp(t,u) eq(t)
           val endEqThrows = V(1, "v")..V(3, "w", "e")
           show("endEqThrows==same") { endEqThrows == same }
                                                      // threw eq:w | cmp(v,w) eq(v) eq(w)
           val endHashThrows = V(1, "k")..V(3, "l", "h")
           show("endHashThrows.hash") { endHashThrows.hashCode() }
                                                      // threw hash:l | cmp(k,l) hash(k) hash(l)
           val startHashThrows = V(1, "m", "h")..V(3, "n", "h")
           show("startHashThrows.hash") { startHashThrows.hashCode() }
                                                      // threw hash:m | cmp(m,n) hash(m)
           val startStrThrows = V(1, "o", "s")..V(3, "y", "s")
           show("startStrThrows.str") { startStrThrows.toString() } // threw str:o | str(o)
           val endStrThrows = V(1, "z")..V(3, "zz", "s")
           show("endStrThrows.str") { endStrThrows.toString() } // threw str:zz | str(z) str(zz)
       }
*/
#include "later_tiers.h"

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

/* Whether `self`'s member `member` throws, and if so, throws. */
static kt_boolean throws(KRef self, char member) {
    V *v = (V *)self;
    for (kt_int at = 0; v->throws[at] != 0; at++) {
        if (v->throws[at] == member) {
            v->thrown = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
            kt_throw(v->thrown);
            return true;
        }
    }
    return false;
}

static kt_int v_compare(KRef self, KRef other) {
    log_call("cmp", self, other);
    if (throws(self, 'c')) {
        return 0;
    }
    kt_int a = ((const V *)self)->n;
    kt_int b = ((const V *)other)->n;
    return a < b ? -1 : a > b ? 1 : 0;
}

static const KType v_type;

static kt_boolean v_equals(KRef self, KRef other) {
    log_call("eq", self, NULL);
    if (throws(self, 'e')) {
        return true;
    }
    return other != NULL && type_of(other) == &v_type &&
           ((const V *)other)->n == ((const V *)self)->n;
}

static kt_int v_hash_code(KRef self) {
    log_call("hash", self, NULL);
    if (throws(self, 'h')) {
        return 0;
    }
    return ((const V *)self)->n;
}

/* `kt_string_utf8` names its bytes without copying, so each rendering is a literal. */
static KRef v_to_string(KRef self) {
    log_call("str", self, NULL);
    if (throws(self, 's')) {
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

/* A member that threw: the exception pending is the one `thrower` threw, the call log is exactly
   `calls`, and the answer returned beside the exception is not looked at. */
static void expect_threw(KRef thrower, const char *calls) {
    CHECK(kt_pending_exception() != NULL && kt_pending_exception() == ((const V *)thrower)->thrown,
          "the pending exception is not the throwing member's\n");
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

    CHECK(kt_equals(a, same) && log_is("cmp(a,b) eq(a) eq(b) "), "a == same\n");
    CHECK(!kt_equals(a, empty1) && log_is("cmp(a,b) eq(a) "), "a == empty1\n");
    CHECK(kt_equals(empty1, empty2) && log_is("cmp(e,f) cmp(g,h) "), "empty1 == empty2\n");
    CHECK(!kt_equals(empty1, a) && log_is("cmp(e,f) cmp(a,b) eq(e) "), "empty1 == a\n");
    CHECK(!kt_equals(a, other) && log_is("cmp(a,b) eq(a) eq(b) "), "a == other\n");
    CHECK(kt_hash_code(a) == 34 && log_is("cmp(a,b) hash(a) hash(b) "), "a.hashCode()\n");
    CHECK(kt_hash_code(empty1) == -1 && log_is("cmp(e,f) "), "empty1.hashCode()\n");
    CHECK(text_is(kt_to_string(a), "V1..V3", 6) && log_is("str(a) str(b) "), "a.toString()\n");
    CHECK(text_is(kt_to_string(empty1), "V5..V2", 6) && log_is("str(e) str(f) "),
          "empty1.toString()\n");
    CHECK(kt_pending_exception() == NULL, "an ordinary member raised\n");

    KRef p = v(1, "p", "c");
    (void)kt_equals(range(p, v(3, "q", "")), a);
    expect_threw(p, "cmp(p,q) ");

    KRef r = v(1, "r", "c");
    (void)kt_equals(empty1, range(r, v(0, "s", "")));
    expect_threw(r, "cmp(e,f) cmp(r,s) ");

    KRef t = v(1, "t", "e");
    (void)kt_equals(range(t, v(3, "u", "")), same);
    expect_threw(t, "cmp(t,u) eq(t) ");

    KRef w = v(3, "w", "e");
    (void)kt_equals(range(v(1, "v", ""), w), same);
    expect_threw(w, "cmp(v,w) eq(v) eq(w) ");

    KRef l = v(3, "l", "h");
    (void)kt_hash_code(range(v(1, "k", ""), l));
    expect_threw(l, "cmp(k,l) hash(k) hash(l) ");

    KRef m = v(1, "m", "h");
    (void)kt_hash_code(range(m, v(3, "n", "h")));
    expect_threw(m, "cmp(m,n) hash(m) ");

    KRef o = v(1, "o", "s");
    (void)kt_to_string(range(o, v(3, "y", "s")));
    expect_threw(o, "str(o) ");

    KRef zz = v(3, "zz", "s");
    (void)kt_to_string(range(v(1, "z", ""), zz));
    expect_threw(zz, "str(z) str(zz) ");

    kt_sys_write(1, "OK\n", 3);
}
