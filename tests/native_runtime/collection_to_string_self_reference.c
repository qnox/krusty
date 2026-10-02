/* A map or set that holds ITSELF renders the marker Kotlin's stdlib writes in its place --
   `(this Map)` for a map's key or value, `(this Collection)` for a set's element -- as
   `AbstractMap` and `AbstractCollection` do. Both used to render the element through its own
   `toString`, which rendered the element again, without end, until the stack ran out. */
#include "later_tiers.h"

#define TEXT(literal) literal, (kt_int)(sizeof(literal) - 1)

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    KRef valued = kt_map_new();
    kt_map_set(valued, kt_string_utf8(TEXT("me")), valued);
    CHECK(text_is(kt_to_string(valued), TEXT("{me=(this Map)}")),
          "a map that is its own value did not render the marker\n");

    KRef keyed = kt_map_new();
    kt_map_set(keyed, keyed, kt_string_utf8(TEXT("v")));
    kt_map_set(keyed, kt_string_utf8(TEXT("k")), kt_string_utf8(TEXT("w")));
    CHECK(text_is(kt_to_string(keyed), TEXT("{(this Map)=v, k=w}")),
          "a map that is its own key did not render the marker\n");

    KRef set = kt_set_new();
    (void)kt_set_add(set, kt_string_utf8(TEXT("a")));
    (void)kt_set_add(set, set);
    CHECK(text_is(kt_to_string(set), TEXT("[a, (this Collection)]")),
          "a set that holds itself did not render the marker\n");

    /* Only the collection ITSELF is replaced: another map inside one renders as it always does. */
    KRef outer = kt_map_new();
    KRef inner = kt_map_new();
    kt_map_set(inner, kt_string_utf8(TEXT("x")), kt_string_utf8(TEXT("y")));
    kt_map_set(outer, kt_string_utf8(TEXT("in")), inner);
    CHECK(text_is(kt_to_string(outer), TEXT("{in={x=y}}")), "a nested map rendered wrong\n");

    CHECK(kt_pending_exception() == NULL, "rendering raised\n");
    kt_sys_write(1, "OK\n", 3);
}
