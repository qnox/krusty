/* A builder whose text would grow past the largest length an `Int` counts ends the program as out
   of memory, which is where Kotlin fails too. Its growth arithmetic used to overflow instead:
   the needed size wrapped negative, passed as "already fits", and the append copied the text over
   whatever followed the storage.

   The appended string is a VIEW claiming nearly 2 GiB over a few real bytes, so the driver never
   allocates what it asks for: the fixed runtime refuses before copying a byte, and the harness
   expects that refusal. This driver must NOT print OK. */
#include "later_tiers.h"

static const char few[16] = "0123456789abcdef";

void kt_program_entry(void) {
    KRef builder = kt_string_builder_with_text(kt_string_utf8(few, 16));
    kt_string_builder_append(builder, kt_string_utf8(few, 0x7ffffff8));
    KT_SYS_FAIL("an append past the largest length came back\n");
}
