/* `StringBuilder` on its ordinary paths: appends of each kind of value, `appendLine`, `toString`
   as a copy the builder's next change leaves alone, `setLength` shorter (including a length that
   falls between the halves of a surrogate pair, which keeps the high half), longer (padding with
   NUL) and zero, growth past the initial capacity, identity equality, and a builder appended to
   itself.

   The expected text is Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       fun main() {
           val sb = StringBuilder()
           sb.append("ab").append(1).append('c').append(null as Any?).append(2.5)
           println("$sb ${sb.length}")
           sb.appendLine("x"); sb.appendLine()
           println(sb.toString().replace("\n", "\\n"))
           val copy = StringBuilder("hello")
           val snap = copy.toString(); copy.append("!")
           println("$snap $copy")
           val sl = StringBuilder("hello"); sl.setLength(2); println("[$sl] ${sl.length}")
           sl.setLength(4); println("${sl.toString().map { it.code }} ${sl.length}")
           sl.setLength(0); println("[$sl] ${sl.length}")
           val em = StringBuilder("a😀b"); em.setLength(2)
           println("${em.toString().map { it.code }} ${em.length}")
           val em2 = StringBuilder("a😀b"); em2.setLength(3)
           println("${em2.toString().map { it.code }} ${em2.length} ${em2.toString() == "a😀"}")
           println("[${StringBuilder(0)}] [${StringBuilder(5).append("past the capacity")}]")
           val b1 = StringBuilder("x"); val b2 = StringBuilder("x"); println("${b1 == b2} ${b1 == b1}")
           val self = StringBuilder("ab"); self.append(self); println(self)
       }

   which prints `ab1cnull2.5 11`, `ab1cnull2.5x\n\n`, `hello hello!`, `[he] 2`,
   `[104, 101, 0, 0] 4`, `[] 0`, `[97, 55357] 2`, `[97, 55357, 56832] 3 true`,
   `[] [past the capacity]`, `false true` and `abab`. A lone surrogate is stored as the three bytes
   its code unit encodes to, so the high half D83D is `ED A0 BD`. */
#include "driver_checks.h"

#define TEXT_IS(text, literal) text_is(text, literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef sb = kt_string_builder_new();
    CHECK(kt_string_builder_append(sb, kt_string_utf8("ab", 2)) == sb,
          "append answers the builder\n");
    (void)kt_string_builder_append(sb, kt_box_int(1));
    (void)kt_string_builder_append(sb, kt_box_char('c'));
    (void)kt_string_builder_append(sb, NULL);
    (void)kt_string_builder_append(sb, kt_box_double(2.5));
    CHECK(TEXT_IS(sb, "ab1cnull2.5") && kt_string_length(sb) == 11, "appends of each kind\n");
    (void)kt_string_builder_append_line(sb, kt_string_utf8("x", 1));
    (void)kt_string_builder_append_new_line(sb);
    CHECK(TEXT_IS(sb, "ab1cnull2.5x\n\n"), "appendLine(x) and appendLine()\n");

    KRef copy = kt_string_builder_with_text(kt_string_utf8("hello", 5));
    KRef snap = kt_to_string(copy);
    CHECK(type_of(snap) == &kt_type_string, "a builder's toString is not a string\n");
    (void)kt_string_builder_append(copy, kt_string_utf8("!", 1));
    CHECK(TEXT_IS(snap, "hello") && TEXT_IS(copy, "hello!"), "toString is not a copy\n");

    KRef sl = kt_string_builder_with_text(kt_string_utf8("hello", 5));
    kt_string_builder_set_length(sl, 2);
    CHECK(TEXT_IS(sl, "he") && kt_string_length(sl) == 2, "setLength(2)\n");
    kt_string_builder_set_length(sl, 4);
    CHECK(text_is(sl, "he\0\0", 4) && kt_string_length(sl) == 4, "setLength(4) pads with NUL\n");
    kt_string_builder_set_length(sl, 0);
    CHECK(TEXT_IS(sl, "") && kt_string_length(sl) == 0, "setLength(0)\n");

    /* "a😀b": 'a', the four-byte character, 'b'. */
    static const char emoji[] = "a\xF0\x9F\x98\x80" "b";
    KRef em = kt_string_builder_with_text(kt_string_utf8(emoji, sizeof(emoji) - 1));
    CHECK(kt_string_length(em) == 4, "a builder holding a pair counts two units for it\n");
    kt_string_builder_set_length(em, 2);
    CHECK(TEXT_IS(em, "a\xED\xA0\xBD") && kt_string_length(em) == 2 &&
              kt_string_get(em, 1) == 0xD83D,
          "setLength between the halves keeps the high half\n");
    KRef em2 = kt_string_builder_with_text(kt_string_utf8(emoji, sizeof(emoji) - 1));
    kt_string_builder_set_length(em2, 3);
    CHECK(TEXT_IS(em2, "a\xF0\x9F\x98\x80") && kt_string_length(em2) == 3,
          "setLength after the pair keeps it whole\n");

    CHECK(TEXT_IS(kt_string_builder_with_capacity(0), ""), "StringBuilder(0)\n");
    KRef grown = kt_string_builder_with_capacity(5);
    (void)kt_string_builder_append(grown, kt_string_utf8("past the capacity", 17));
    CHECK(TEXT_IS(grown, "past the capacity"), "a builder did not grow past its capacity\n");

    KRef b1 = kt_string_builder_with_text(kt_string_utf8("x", 1));
    KRef b2 = kt_string_builder_with_text(kt_string_utf8("x", 1));
    CHECK(!kt_equals(b1, b2) && kt_equals(b1, b1), "builders are equal by identity\n");

    KRef self = kt_string_builder_with_text(kt_string_utf8("ab", 2));
    (void)kt_string_builder_append(self, self);
    CHECK(TEXT_IS(self, "abab"), "a builder appended to itself\n");

    CHECK(kt_pending_exception() == NULL, "a builder operation raised\n");
    kt_sys_write(1, "OK\n", 3);
}
