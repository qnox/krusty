/* The transcript a driver prints for the harness to compare with Kotlin's.

   A driver whose expected answers are Kotlin's does not copy those answers into C. It prints what
   the runtime answered, one observation per line, and the Kotlin program beside it
   (`tests/native_runtime/<driver>.kt`) builds the same lines in its `box()`. The harness compiles
   that program with the reference kotlinc, runs it on the JVM, and requires the driver's transcript
   to equal it exactly (`run_driver_against_kotlin` in `tests/native_runtime_e2e.rs`), so every
   answer is Kotlin's when the test runs rather than when someone last read it off a terminal.

   A line renders a runtime object through the runtime's own `toString` wherever that rendering is
   part of the claim; an integer the claim is not about the rendering of is written here. What has
   no Kotlin counterpart -- which object is pending, the exact calls made into the program, the
   message the runtime ends a program with -- stays a `CHECK` in the driver. The transcript ends
   before the driver's `OK`. Include this from exactly one file per driver, as `driver_checks.h`. */
#ifndef KRUSTY_TEST_TRANSCRIPT_H
#define KRUSTY_TEST_TRANSCRIPT_H

#include "driver_checks.h"

/* `length` bytes, as they are. */
static inline void say_bytes(const char *bytes, kt_int length) {
    kt_sys_write(1, bytes, (size_t)length);
}

/* A C string, as it is: a label, a separator, a bracket. */
static inline void say(const char *text) {
    kt_int length = 0;
    while (text[length] != 0) {
        length++;
    }
    say_bytes(text, length);
}

/* The text of a string (or builder) the runtime handed back. */
static inline void say_text(KRef text) {
    kt_int length = 0;
    const char *bytes = driver_text_of(text, &length);
    say_bytes(bytes, length);
}

/* `value.toString()` as the runtime renders it; `null` renders as `null`. A rendering that raises
   is not an observation of the claim, so it ends the driver. */
static inline void say_value(KRef value) {
    KRef text = kt_to_string(value);
    CHECK(kt_pending_exception() == NULL, "rendering an observation raised\n");
    say_text(text);
}

static inline void say_bool(kt_boolean value) { say(value ? "true" : "false"); }

static inline void say_ulong(uint64_t value) {
    char digits[20];
    kt_int at = sizeof(digits);
    do {
        digits[--at] = (char)('0' + value % 10);
        value /= 10;
    } while (value != 0);
    say_bytes(digits + at, (kt_int)sizeof(digits) - at);
}

static inline void say_long(kt_long value) {
    if (value < 0) {
        say("-");
        say_ulong(0 - (uint64_t)value);
        return;
    }
    say_ulong((uint64_t)value);
}

/* The simple name of a descriptor: its name after the last dot, as `::class.simpleName` answers
   for the classes a driver names this way. */
static inline void say_simple_name(const KType *type) {
    kt_int start = 0;
    for (kt_int at = 0; at < (kt_int)type->name_length; at++) {
        if (type->name[at] == '.') {
            start = at + 1;
        }
    }
    say_bytes(type->name + start, (kt_int)type->name_length - start);
}

/* `threw <SimpleName>: <message>`, what a Kotlin program writes for an exception it caught. The
   message is read as it is, so this may run while the exception is still pending. */
static inline void say_thrown(KRef thrown) {
    say("threw ");
    say_simple_name(type_of(thrown));
    say(": ");
    KRef message = kt_throwable_message(thrown);
    if (message == NULL) {
        say("null");
    } else {
        say_text(message);
    }
}

/* `label = value` and the line's end, the value as a string template renders it. */
static inline void say_line(const char *label, KRef value) {
    say(label);
    say(" = ");
    say_value(value);
    say("\n");
}

/* `SimpleName: message`, as `"${e::class.simpleName}: ${e.message}"` renders a throwable. */
static inline void say_throwable(KRef thrown) {
    say_simple_name(type_of(thrown));
    say(": ");
    say_value(kt_throwable_message(thrown));
}

/* `label = SimpleName: message` for the exception in flight, which it takes, as
   `"${e::class.simpleName}: ${e.message}"` renders one; `label = <none>` when nothing is, which no
   Kotlin program answers. */
static inline void say_raised(const char *label) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    say(label);
    say(" = ");
    if (thrown == NULL) {
        say("<none>\n");
        return;
    }
    say_throwable(thrown);
    say("\n");
}

/* `length` bytes of a descriptor's name, as they are. */
static inline void say_bytes_of(const char *bytes, uint32_t length) {
    say_bytes(bytes, (kt_int)length);
}

/* An exception class of the program's own, `class <simple> : Exception(...)`, as the generator
   declares one: the throwable's layout, `Exception` its superclass, its names published. */
#define PROGRAM_EXCEPTION(identifier, simple)                                                      \
    static const kt_fn identifier##_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,    \
                                                (kt_fn)kt_throwable_to_string};                    \
    static const KType identifier = {                                                              \
        .name = simple,                                                                            \
        .name_length = sizeof(simple) - 1,                                                         \
        .instance_size = sizeof(DriverThrowable),                                                  \
        .reference_count = 2,                                                                      \
        .reference_offsets = driver_throwable_offsets,                                             \
        .super = &kt_type_exception,                                                               \
        .vtable = identifier##_vtable,                                                             \
        .vtable_length = 3,                                                                        \
        .qualified_name = simple,                                                                  \
        .qualified_name_length = sizeof(simple) - 1,                                               \
        .simple_name = simple,                                                                     \
        .simple_name_length = sizeof(simple) - 1,                                                  \
        .class_names = KT_CLASS_NAMES_MEMBER};

#endif
