/* The stdlib's native String searches: `indexOf` by a `String` or a `Char`, and `lastIndexOf` by a
   `Char`, which the Native stdlib declares as `nativeIndexOf` and `nativeLastIndexOf` externals.
   Every offset counts UTF-16 units, as the language does. */
#include "krusty_internal.h"

/* Kotlin/Native's `Kotlin_String_indexOfString`: the first unit offset at or after `from` where
   `other` occurs in `self`. A negative start counts from zero; a start at or past the end finds
   only an empty `other`, at the end. */
kt_int kt_string_index_of_string(KRef self, KRef other, kt_int from) {
    kt_int length = kt_string_length(self);
    kt_int wanted = kt_string_length(other);
    if (from < 0) {
        from = 0;
    }
    if (from >= length) {
        return wanted == 0 ? length : -1;
    }
    if (wanted == 0) {
        return from;
    }
    /* One cursor walks the receiver for the whole search, so a candidate costs the units it
       compares rather than the prefix before it. Each comparison runs on copies of the walks. */
    KUnits cursor = kt_units_of(self);
    KUnits needle = kt_units_of(other);
    kt_char unit = 0;
    for (kt_int skipped = 0; skipped < from; skipped++) {
        (void)kt_units_next(&cursor, &unit);
    }
    for (kt_int at = from; at + wanted <= length; at++) {
        KUnits text = cursor;
        KUnits sought = needle;
        kt_char have = 0;
        kt_char want = 0;
        kt_boolean matches = 1;
        while (kt_units_next(&sought, &want)) {
            if (!kt_units_next(&text, &have) || have != want) {
                matches = 0;
                break;
            }
        }
        if (matches) {
            return at;
        }
        (void)kt_units_next(&cursor, &unit);
    }
    return -1;
}

/* Kotlin/Native's `Kotlin_String_indexOfChar`: the first unit offset at or after `from` holding
   `ch`, or -1. */
kt_int kt_string_index_of_char(KRef self, kt_char ch, kt_int from) {
    KUnits units = kt_units_of(self);
    kt_char unit = 0;
    kt_int at = 0;
    if (from < 0) {
        from = 0;
    }
    while (kt_units_next(&units, &unit)) {
        if (at >= from && unit == ch) {
            return at;
        }
        at++;
    }
    return -1;
}

/* Kotlin/Native's `Kotlin_String_lastIndexOfChar`: the last unit offset at or before `from` holding
   `ch`, or -1; a negative `from` finds nothing, and one past the end starts at the last unit. */
kt_int kt_string_last_index_of_char(KRef self, kt_char ch, kt_int from) {
    KUnits units = kt_units_of(self);
    kt_char unit = 0;
    kt_int at = 0;
    kt_int found = -1;
    if (from < 0) {
        return -1;
    }
    while (at <= from && kt_units_next(&units, &unit)) {
        if (unit == ch) {
            found = at;
        }
        at++;
    }
    return found;
}
