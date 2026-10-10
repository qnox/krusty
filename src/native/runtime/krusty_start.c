/* krusty native runtime: the process boundary. `_start`, the arguments and environment the kernel
   hands it, standard input, and the end of the process. Hand-written freestanding C; build.rs
   compiles it into the runtime for every target. */
#include "krusty_internal.h"

void kt_program_entry(void);

/* What the kernel left on the initial stack, read once by `kt_process_start`: the argument vector
   and, after its terminating NULL, the environment's. */
static kt_int kt_argc;
static char **kt_argv;
static char **kt_envp;

/* Where `_start` goes: record the initial stack, run the program, and end the process with status
   0, as a Kotlin `main` that returns does. Ending here rather than returning matters because there
   is nothing to return to: the instruction after the call would trap, reporting a normal end as a
   crash. */
__attribute__((noreturn, used)) void kt_process_start(long *initial_stack) {
    /* The System V process-start layout every supported architecture shares: argc, then argv's
       pointers and a NULL, then envp's pointers and a NULL. */
    kt_argc = (kt_int)initial_stack[0];
    kt_argv = (char **)(initial_stack + 1);
    kt_envp = kt_argv + kt_argc + 1;
    kt_program_entry();
    kt_sys_exit(0);
}

#if defined(__x86_64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  xorl %ebp, %ebp\n"
        "  movq %rsp, %rdi\n"
        "  andq $-16, %rsp\n"
        "  call kt_process_start\n");
#elif defined(__aarch64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  mov x29, #0\n"
        "  mov x30, #0\n"
        "  mov x0, sp\n"
        "  bl kt_process_start\n");
#elif defined(__riscv) && __riscv_xlen == 64
__asm__(".globl _start\n"
        "_start:\n"
        "  li s0, 0\n"
        "  li ra, 0\n"
        "  mv a0, sp\n"
        "  call kt_process_start\n");
#else
#error "krusty native: unsupported architecture"
#endif

/* ---- text from outside ------------------------------------------------------------------------

   An argument or a line of input is bytes the program did not write, and a `String` must hold
   well-formed UTF-8: every walk over one trusts it. Each maximal ill-formed subpart becomes U+FFFD,
   which is what the JVM's decoder and Kotlin/Native's `decodeToString` both answer. */

/* The length of the well-formed sequence at `bytes[0]`, or 0 when it is ill-formed. On 0, `*bad` is
   how many bytes the ill-formed subpart spans, each later one a continuation the lead allowed. */
static size_t kt_utf8_sequence(const unsigned char *bytes, size_t available, size_t *bad) {
    unsigned char lead = bytes[0];
    *bad = 1;
    if (lead < 0x80) {
        return 1;
    }
    size_t length;
    unsigned char low = 0x80, high = 0xBF;
    if (lead >= 0xC2 && lead <= 0xDF) {
        length = 2;
    } else if (lead >= 0xE0 && lead <= 0xEF) {
        length = 3;
        /* No overlong forms, and no surrogates: those are UTF-16's, never text. */
        if (lead == 0xE0) {
            low = 0xA0;
        } else if (lead == 0xED) {
            high = 0x9F;
        }
    } else if (lead >= 0xF0 && lead <= 0xF4) {
        length = 4;
        if (lead == 0xF0) {
            low = 0x90;
        } else if (lead == 0xF4) {
            high = 0x8F;
        }
    } else {
        return 0;
    }
    for (size_t index = 1; index < length; index++) {
        if (index >= available) {
            return 0;
        }
        unsigned char next = bytes[index];
        if (next < low || next > high) {
            return 0;
        }
        low = 0x80;
        high = 0xBF;
        *bad = index + 1;
    }
    return length;
}

/* A `String` holding `bytes` decoded as UTF-8. */
static KRef kt_string_decoded(const char *bytes, size_t length) {
    const unsigned char *in = (const unsigned char *)bytes;
    size_t decoded = 0;
    for (size_t at = 0; at < length;) {
        size_t bad;
        size_t step = kt_utf8_sequence(in + at, length - at, &bad);
        decoded += step != 0 ? step : 3;
        at += step != 0 ? step : bad;
    }
    if (decoded > 0x7FFFFFFF) {
        kt_fail_oom();
    }
    KByteArray *storage = kt_bytes_new((kt_int)decoded);
    char *out = kt_bytes_of(storage);
    size_t written = 0;
    for (size_t at = 0; at < length;) {
        size_t bad;
        size_t step = kt_utf8_sequence(in + at, length - at, &bad);
        if (step != 0) {
            memcpy(out + written, in + at, step);
            written += step;
            at += step;
        } else {
            out[written++] = (char)0xEF;
            out[written++] = (char)0xBF;
            out[written++] = (char)0xBD;
            at += bad;
        }
    }
    return kt_string_of((KRef)storage, out, (kt_int)decoded);
}

static size_t kt_c_length(const char *text) {
    size_t length = 0;
    while (text[length] != '\0') {
        length++;
    }
    return length;
}

/* ---- arguments --------------------------------------------------------------------------------- */

KRef kt_program_arguments(void) {
    /* Kotlin's `args` excludes the program's own name, which the kernel passes as `argv[0]`. */
    kt_int count = kt_argc > 0 ? kt_argc - 1 : 0;
    KRef array = kt_array_new(&kt_type_array, count);
    for (kt_int index = 0; index < count; index++) {
        const char *argument = kt_argv[index + 1];
        /* `array` is live in this frame across the allocation, which is what roots it. */
        KRef text = kt_string_decoded(argument, kt_c_length(argument));
        kt_elements_of(array)[index] = text;
    }
    return array;
}

/* ---- standard input ----------------------------------------------------------------------------

   One buffer, refilled by `read(2)`, so a line costs a system call per buffer rather than per byte.
   Nothing else in the process reads descriptor 0 through the runtime, which is what makes holding
   bytes past the line just returned safe. */

#define KT_STDIN_BUFFER 4096
static char kt_stdin_buffer[KT_STDIN_BUFFER];
static size_t kt_stdin_start;
static size_t kt_stdin_end;
static bool kt_stdin_ended;

static bool kt_stdin_fill(void) {
    while (!kt_stdin_ended) {
        long got = kt_sys_read(0, kt_stdin_buffer, KT_STDIN_BUFFER);
        if (got == -KT_EINTR) {
            continue;
        }
        if (got <= 0) {
            /* End of input, or an error, which Kotlin's console treats the same way: there is no
               more to read. */
            kt_stdin_ended = true;
            return false;
        }
        kt_stdin_start = 0;
        kt_stdin_end = (size_t)got;
        return true;
    }
    return false;
}

/* A line accumulated across refills: the line may be longer than the buffer. Mapped rather than
   allocated on the heap, because it is scratch that never becomes a Kotlin value itself. */
static char *kt_line;
static size_t kt_line_length;
static size_t kt_line_capacity;

static void kt_line_append(const char *bytes, size_t length) {
    if (kt_line_length + length > kt_line_capacity) {
        size_t grown = kt_line_capacity == 0 ? KT_STDIN_BUFFER : kt_line_capacity;
        while (grown < kt_line_length + length) {
            grown *= 2;
        }
        char *replacement = (char *)kt_map(grown);
        if (kt_line != NULL) {
            memcpy(replacement, kt_line, kt_line_length);
            kt_unmap(kt_line, kt_line_capacity);
        }
        kt_line = replacement;
        kt_line_capacity = grown;
    }
    memcpy(kt_line + kt_line_length, bytes, length);
    kt_line_length += length;
}

KRef kt_read_line(void) {
    kt_line_length = 0;
    bool read_any = false;
    for (;;) {
        if (kt_stdin_start == kt_stdin_end && !kt_stdin_fill()) {
            if (!read_any) {
                return NULL;
            }
            break;
        }
        read_any = true;
        const char *from = kt_stdin_buffer + kt_stdin_start;
        size_t available = kt_stdin_end - kt_stdin_start;
        size_t newline = 0;
        while (newline < available && from[newline] != '\n') {
            newline++;
        }
        kt_line_append(from, newline);
        if (newline < available) {
            kt_stdin_start += newline + 1;
            /* `\r\n` ends a line as `\n` does; a `\r` anywhere else is the line's own. */
            if (kt_line_length > 0 && kt_line[kt_line_length - 1] == '\r') {
                kt_line_length--;
            }
            break;
        }
        kt_stdin_start = kt_stdin_end;
    }
    return kt_string_decoded(kt_line, kt_line_length);
}

KRef kt_read_line_or_throw(void) {
    KRef line = kt_read_line();
    if (line == NULL) {
        kt_throw(kt_throwable_new(&kt_type_read_after_eof_exception,
                                  kt_string_utf8("EOF has already been reached", 28)));
    }
    return line;
}
