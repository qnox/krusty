/* `Pair`'s ordinary members when no component throws: `first` and `second` by identity, `equals`
   componentwise through each component's own `equals` and only against another `Pair`, `hashCode`
   as `first.hashCode() * 31 + second.hashCode()` with a null component counting 0 (in 32-bit
   arithmetic that wraps), and `toString` as `(first, second)` through each component's own
   `toString`.

   The expected answers are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       class Tag(val n: Int) {
           override fun equals(other: Any?) = other is Tag && other.n == n
           override fun hashCode() = n
           override fun toString() = "T$n"
       }
       fun main() {
           val p = Tag(1) to Tag(2)
           println("${p.first} ${p.second} $p ${p.hashCode()}")
           println("${p == (Tag(1) to Tag(2))} ${p == (Tag(1) to Tag(3))} ${p == (Tag(2) to Tag(2))}")
           println("${p.equals(null)} ${p.equals(Tag(1))}")
           val q = Tag(7) to null
           val r = null to Tag(7)
           val z = null to null
           println("$q ${q.hashCode()} $r ${r.hashCode()} $z ${z.hashCode()} ${z == (null to null)}")
           println("${(Tag(Int.MAX_VALUE) to Tag(1)).hashCode()}")
       }

   which prints `T1 T2 (T1, T2) 33`, `true false false`, `false false`,
   `(T7, null) 217 (null, T7) 7 (null, null) 0 true` and `2147483618`. */
#include "later_tiers.h"

typedef struct Tag {
    KObjectHeader header;
    kt_int n;
} Tag;

static const KType tag_type;

static kt_boolean tag_equals(KRef self, KRef other) {
    return other != NULL && type_of(other) == &tag_type &&
           ((const Tag *)other)->n == ((const Tag *)self)->n;
}

static kt_int tag_hash_code(KRef self) { return ((const Tag *)self)->n; }

/* `kt_string_utf8` names the bytes it is given without copying them, so each rendering is a
   literal; only the tags this driver renders need one. */
static KRef tag_to_string(KRef self) {
    switch (((const Tag *)self)->n) {
    case 1:
        return kt_string_utf8("T1", 2);
    case 2:
        return kt_string_utf8("T2", 2);
    case 7:
        return kt_string_utf8("T7", 2);
    default:
        KT_SYS_FAIL("a tag this driver does not render was rendered\n");
        return NULL;
    }
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

static KRef tag(kt_int n) {
    Tag *made = (Tag *)kt_gc_allocate(&tag_type, sizeof(Tag));
    made->n = n;
    return (KRef)made;
}

static kt_boolean renders(KRef value, const char *bytes, kt_int length) {
    return text_is(kt_to_string(value), bytes, length);
}

#define RENDERS(value, literal) renders(value, literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef one = tag(1);
    KRef two = tag(2);
    KRef p = kt_pair_of(one, two);
    CHECK(type_of(p) == &kt_type_pair, "a pair's type\n");
    CHECK(kt_pair_first(p) == one && kt_pair_second(p) == two, "first and second\n");
    CHECK(RENDERS(p, "(T1, T2)"), "(Tag(1) to Tag(2)).toString()\n");
    CHECK(kt_hash_code(p) == 33, "(Tag(1) to Tag(2)).hashCode()\n");

    CHECK(kt_equals(p, p), "a pair is not equal to itself\n");
    CHECK(kt_equals(p, kt_pair_of(tag(1), tag(2))), "equal components compare unequal\n");
    CHECK(!kt_equals(p, kt_pair_of(tag(1), tag(3))), "a different second compares equal\n");
    CHECK(!kt_equals(p, kt_pair_of(tag(2), tag(2))), "a different first compares equal\n");
    CHECK(!kt_equals(p, NULL), "p.equals(null)\n");
    CHECK(!kt_equals(p, tag(1)), "p.equals(Tag(1))\n");

    KRef q = kt_pair_of(tag(7), NULL);
    CHECK(RENDERS(q, "(T7, null)") && kt_hash_code(q) == 217, "Tag(7) to null\n");
    KRef r = kt_pair_of(NULL, tag(7));
    CHECK(RENDERS(r, "(null, T7)") && kt_hash_code(r) == 7, "null to Tag(7)\n");
    KRef z = kt_pair_of(NULL, NULL);
    CHECK(RENDERS(z, "(null, null)") && kt_hash_code(z) == 0, "null to null\n");
    CHECK(kt_equals(z, kt_pair_of(NULL, NULL)), "(null to null) == (null to null)\n");

    CHECK(kt_hash_code(kt_pair_of(tag(0x7fffffff), tag(1))) == 2147483618,
          "a hash that wraps\n");

    CHECK(kt_pending_exception() == NULL, "a pair member raised\n");
    kt_sys_write(1, "OK\n", 3);
}
