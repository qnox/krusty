/* A program may write more distinct string literals than the collector has global root slots —
   4096, shared with every top-level property. Interning each literal used to register its slot as a
   root of its own, so the 4097th distinct literal ended the program with "too many global roots".
   The literals are now kept alive by ONE table the runtime owns, and each must still survive a
   collection with its text and its identity intact. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define LITERALS 5000

static KRef slots[LITERALS];

/* Each literal's text, in static storage as a literal's is: a string made from bytes the program
   owns refers to them rather than copying them. */
static char texts[LITERALS][4];
static kt_int lengths[LITERALS];

/* The decimal digits of `value`, which makes every literal's text distinct. */
static kt_int text_of(unsigned value, char *out) {
    char digits[4];
    kt_int count = 0;
    do {
        digits[count++] = (char)('0' + value % 10u);
        value /= 10u;
    } while (value != 0);
    for (kt_int i = 0; i < count; i++) {
        out[i] = digits[count - 1 - i];
    }
    return count;
}

__attribute__((noinline)) static void intern_all(void) {
    for (unsigned i = 0; i < LITERALS; i++) {
        lengths[i] = text_of(i, texts[i]);
        if (kt_string_literal(texts[i], lengths[i], &slots[i]) != slots[i]) {
            KT_SYS_FAIL("a literal did not answer the string its slot holds\n");
        }
    }
}

/* Enough garbage that a swept literal's storage would be handed out again and overwritten. */
__attribute__((noinline)) static void churn(void) {
    for (unsigned i = 0; i < 4 * LITERALS; i++) {
        kt_string_utf8("garbage!", 8);
    }
}

__attribute__((noinline)) static void check_all(void) {
    char text[4];
    for (unsigned i = 0; i < LITERALS; i++) {
        KRef before = slots[i];
        if (kt_string_literal(texts[i], lengths[i], &slots[i]) != before) {
            KT_SYS_FAIL("a literal answered a different object on its second use\n");
        }
        /* A literal the collector did not keep has had its storage handed out again by the churn,
           and reads here as whatever took it. */
        kt_int length = text_of(i, text);
        if (!kt_equals(before, kt_string_utf8(text, length))) {
            KT_SYS_FAIL("a literal's text did not survive a collection\n");
        }
    }
}

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    intern_all();
    kt_gc_collect();
    churn();
    kt_gc_collect();
    check_all();
    kt_sys_write(1, "OK\n", 3);
}
