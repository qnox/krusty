//! The C runtime the native backend emits against.
//!
//! **It is freestanding: it does not use a C library.** That is not minimalism for its own sake —
//! it is what makes `docs/BUILD_AND_NATIVE_PLAN.md`'s cross-compilation requirement achievable. Go's
//! defining build property is that `GOOS=linux GOARCH=arm64 go build` works on any machine with
//! nothing installed, because nothing in a Go binary needs a target C toolchain. A runtime that
//! called `printf` would need a target libc, its headers and a target linker for every architecture
//! — the per-target toolchain problem Go exists to avoid. Talking to the kernel directly removes it:
//! `clang --target=<triple>` compiles any registered architecture with no sysroot, and `ld.lld`
//! links any of them, so one host produces binaries for all of them.
//!
//! Only the compiler-provided freestanding headers are used (`stdint.h`, `stddef.h`, `stdbool.h`),
//! which C11 §4 guarantees exist without a hosted implementation.
//!
//! Three limits are deliberate and must not be mistaken for oversights:
//!
//! * **Nothing is ever freed.** Allocation is a bump pointer over `mmap`ed chunks. A `main` that
//!   prints and exits does not care, and choosing a memory-management strategy is a real design
//!   decision (`docs/BUILD_AND_NATIVE_PLAN.md`, phase 8) that a placeholder would prejudge badly.
//! * **`String` is UTF-8 bytes.** Kotlin's `String.length` counts UTF-16 code units, which is not
//!   the byte count for any non-ASCII text. The runtime therefore exposes no `length` at all rather
//!   than exposing a wrong one.
//! * **Floating-point values cannot be rendered.** Kotlin's `Double.toString` is Java's
//!   shortest-round-trip algorithm — `1.0` prints as `1.0` and `1e20` as `1.0E20` — which `printf`'s
//!   `%g` is not, so the libc version was already producing strings Kotlin never would. There is
//!   consequently no `kt_box_double`, which means a `Double` cannot reach a reference position at
//!   all: the backend declines `println(1.0)` at COMPILE time instead of printing something wrong.
//!   Arithmetic and comparison on floating-point values are unaffected.

/// The header emitted code includes.
pub const HEADER: &str = r#"/* krusty native runtime — generated; do not edit. */
#ifndef KRUSTY_RT_H
#define KRUSTY_RT_H

/* Freestanding headers only: C11 guarantees these exist with no C library present, which is what
   lets one host compile for every target architecture without a sysroot. */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef int8_t   kt_byte;
typedef int16_t  kt_short;
typedef int32_t  kt_int;
typedef int64_t  kt_long;
typedef uint16_t kt_char;
typedef float    kt_float;
typedef double   kt_double;
typedef bool     kt_boolean;

/* Every Kotlin reference is one of these. `NULL` is Kotlin's `null`. */
typedef struct KObject KObject;
typedef KObject *KRef;

/* Construct a `String` over a UTF-8 literal. The bytes are borrowed, not copied: emitted code only
   ever passes string literals with static storage duration. */
KRef kt_string_utf8(const char *bytes, kt_int byte_length);

/* `a + b` on strings, after both operands have been rendered. */
KRef kt_string_plus(KRef a, KRef b);

/* `Any?.toString()` — also what a string template calls on each interpolated value. */
KRef kt_to_string(KRef value);

/* No `kt_box_float`/`kt_box_double`: a floating-point value has no renderable form yet, so it must
   not be able to reach a position where something would try to render it. */
KRef kt_box_byte(kt_byte value);
KRef kt_box_short(kt_short value);
KRef kt_box_int(kt_int value);
KRef kt_box_long(kt_long value);
KRef kt_box_char(kt_char value);
KRef kt_box_boolean(kt_boolean value);

kt_byte    kt_unbox_byte(KRef value);
kt_short   kt_unbox_short(KRef value);
kt_int     kt_unbox_int(KRef value);
kt_long    kt_unbox_long(KRef value);
kt_char    kt_unbox_char(KRef value);
kt_boolean kt_unbox_boolean(KRef value);

/* Integer division, remainder and shifts. Kotlin defines all three; C leaves the interesting cases
   undefined. Division by zero throws in Kotlin and is undefined in C; `Int.MIN_VALUE / -1` overflows
   and is undefined in C but wraps in Kotlin; and a shift count outside 0..31 is undefined in C while
   Kotlin masks it to the low five (or six) bits. */
kt_int  kt_div_int(kt_int a, kt_int b);
kt_int  kt_rem_int(kt_int a, kt_int b);
kt_long kt_div_long(kt_long a, kt_long b);
kt_long kt_rem_long(kt_long a, kt_long b);
kt_int  kt_shl_int(kt_int a, kt_int bits);
kt_int  kt_shr_int(kt_int a, kt_int bits);
kt_int  kt_ushr_int(kt_int a, kt_int bits);
kt_long kt_shl_long(kt_long a, kt_int bits);
kt_long kt_shr_long(kt_long a, kt_int bits);
kt_long kt_ushr_long(kt_long a, kt_int bits);

/* `compareTo` on scalars. Separate functions rather than an emitted `a < b ? -1 : ...` so neither
   operand is evaluated twice, and so the floating-point cases can implement Kotlin's TOTAL order
   (NaN above everything, -0.0 below 0.0) rather than C's comparison operators. */
kt_int kt_compare_byte(kt_byte a, kt_byte b);
kt_int kt_compare_short(kt_short a, kt_short b);
kt_int kt_compare_int(kt_int a, kt_int b);
kt_int kt_compare_long(kt_long a, kt_long b);
kt_int kt_compare_char(kt_char a, kt_char b);
kt_int kt_compare_boolean(kt_boolean a, kt_boolean b);
kt_int kt_compare_float(kt_float a, kt_float b);
kt_int kt_compare_double(kt_double a, kt_double b);

/* The `kotlin.Unit` singleton. */
KRef kt_unit(void);

/* kotlin.io. The scalar overloads exist because Kotlin's do: `println(1)` selects `println(Int)`,
   and routing it through the `Any?` overload would box for no reason. */
void kt_print_any(KRef value);
void kt_print_byte(kt_byte value);
void kt_print_short(kt_short value);
void kt_print_int(kt_int value);
void kt_print_long(kt_long value);
void kt_print_char(kt_char value);
void kt_print_boolean(kt_boolean value);

void kt_println_any(KRef value);
void kt_println_byte(kt_byte value);
void kt_println_short(kt_short value);
void kt_println_int(kt_int value);
void kt_println_long(kt_long value);
void kt_println_char(kt_char value);
void kt_println_boolean(kt_boolean value);
void kt_println_unit(void);

/* The generated entry point calls this after running the program's `main`. */
void kt_exit(kt_int status);

#endif /* KRUSTY_RT_H */
"#;

/// The runtime implementation.
pub const SOURCE: &str = r#"/* krusty native runtime — generated; do not edit. */
#include "krusty_rt.h"

/* ---- kernel interface ---------------------------------------------------------------------- */

/* One syscall shim per supported architecture. This is the whole of the target-specific surface:
   everything above it is portable C, which is why adding an architecture is a matter of adding a
   register convention and two numbers rather than porting a runtime. */

#if defined(__x86_64__)
#define KT_SYS_WRITE 1
#define KT_SYS_MMAP 9
#define KT_SYS_EXIT 231 /* exit_group */
#elif defined(__aarch64__) || (defined(__riscv) && __riscv_xlen == 64)
#define KT_SYS_WRITE 64
#define KT_SYS_MMAP 222
#define KT_SYS_EXIT 94 /* exit_group */
#else
#error "krusty native: unsupported architecture"
#endif

static long kt_syscall(long number, long a0, long a1, long a2, long a3, long a4, long a5) {
#if defined(__x86_64__)
    /* The syscall ABI passes the fourth argument in r10, not rcx: `syscall` clobbers rcx. */
    register long r10 __asm__("r10") = a3;
    register long r8 __asm__("r8") = a4;
    register long r9 __asm__("r9") = a5;
    long result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return result;
#elif defined(__aarch64__)
    register long x8 __asm__("x8") = number;
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x5 __asm__("x5") = a5;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5)
                     : "memory");
    return x0;
#else /* riscv64 */
    register long a7r __asm__("a7") = number;
    register long a0r __asm__("a0") = a0;
    register long a1r __asm__("a1") = a1;
    register long a2r __asm__("a2") = a2;
    register long a3r __asm__("a3") = a3;
    register long a4r __asm__("a4") = a4;
    register long a5r __asm__("a5") = a5;
    __asm__ volatile("ecall"
                     : "+r"(a0r)
                     : "r"(a7r), "r"(a1r), "r"(a2r), "r"(a3r), "r"(a4r), "r"(a5r)
                     : "memory");
    return a0r;
#endif
}

void kt_exit(kt_int status) {
    kt_syscall(KT_SYS_EXIT, status, 0, 0, 0, 0, 0);
    __builtin_unreachable();
}

static void kt_write(kt_int fd, const char *bytes, size_t length) {
    size_t written = 0;
    while (written < length) {
        long step = kt_syscall(KT_SYS_WRITE, fd, (long)(bytes + written), (long)(length - written),
                               0, 0, 0);
        /* A short write is normal; anything negative is an error there is nothing useful to do
           about while printing. */
        if (step <= 0) {
            return;
        }
        written += (size_t)step;
    }
}

static void kt_fail(const char *message, size_t length) {
    kt_write(2, message, length);
    kt_exit(134); /* what a SIGABRT exit looks like to a shell */
}

#define KT_FAIL(literal) kt_fail(literal, sizeof(literal) - 1)

/* ---- freestanding C support --------------------------------------------------------------- */

/* A compiler may synthesize calls to these from ordinary assignments and loops even under
   -ffreestanding, so they have to exist as real symbols. */
void *memcpy(void *destination, const void *source, size_t length) {
    unsigned char *out = (unsigned char *)destination;
    const unsigned char *in = (const unsigned char *)source;
    for (size_t index = 0; index < length; index++) {
        out[index] = in[index];
    }
    return destination;
}

void *memset(void *destination, int value, size_t length) {
    unsigned char *out = (unsigned char *)destination;
    for (size_t index = 0; index < length; index++) {
        out[index] = (unsigned char)value;
    }
    return destination;
}

/* ---- allocation ---------------------------------------------------------------------------- */

/* A bump pointer over anonymous mappings. Nothing is freed; see the module comment. */

#define KT_CHUNK (1u << 20)

static char *kt_bump = NULL;
static size_t kt_remaining = 0;

static void *kt_alloc(size_t size) {
    size = (size + 15u) & ~(size_t)15u; /* keep every object suitably aligned */
    if (size > kt_remaining) {
        size_t request = size > KT_CHUNK ? size : KT_CHUNK;
        /* PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, no file. */
        long mapped = kt_syscall(KT_SYS_MMAP, 0, (long)request, 3, 0x22, -1, 0);
        if (mapped <= 0 && mapped >= -4095) {
            KT_FAIL("krusty: out of memory\n");
        }
        kt_bump = (char *)mapped;
        kt_remaining = request;
    }
    void *memory = kt_bump;
    kt_bump += size;
    kt_remaining -= size;
    return memory;
}

/* ---- values -------------------------------------------------------------------------------- */

typedef enum {
    KT_STRING,
    KT_BYTE,
    KT_SHORT,
    KT_INT,
    KT_LONG,
    KT_CHAR,
    KT_BOOLEAN,
    KT_UNIT
} kt_tag;

struct KObject {
    kt_tag tag;
    union {
        struct {
            const char *bytes;
            kt_int byte_length;
        } string;
        kt_byte byte_value;
        kt_short short_value;
        kt_int int_value;
        kt_long long_value;
        kt_char char_value;
        kt_boolean boolean_value;
    } as;
};

static KRef kt_new(kt_tag tag) {
    KRef object = (KRef)kt_alloc(sizeof(KObject));
    object->tag = tag;
    return object;
}

static KRef kt_string_of(const char *bytes, kt_int byte_length) {
    KRef object = kt_new(KT_STRING);
    object->as.string.bytes = bytes;
    object->as.string.byte_length = byte_length;
    return object;
}

KRef kt_string_utf8(const char *bytes, kt_int byte_length) {
    return kt_string_of(bytes, byte_length);
}

/* Render a signed 64-bit value into `buffer` (at least 20 bytes); returns the length written. */
static kt_int kt_render_long(kt_long value, char *buffer) {
    char digits[20];
    kt_int count = 0;
    /* Negate into UNSIGNED space: `-Long.MIN_VALUE` does not exist as a signed value. */
    uint64_t magnitude = value < 0 ? (0u - (uint64_t)value) : (uint64_t)value;
    do {
        digits[count++] = (char)('0' + (magnitude % 10u));
        magnitude /= 10u;
    } while (magnitude != 0);

    kt_int length = 0;
    if (value < 0) {
        buffer[length++] = '-';
    }
    while (count > 0) {
        buffer[length++] = digits[--count];
    }
    return length;
}

/* A `Char` is one UTF-16 code unit; the BMP subset encodes directly as UTF-8. */
static kt_int kt_render_char(kt_char unit, char *buffer) {
    if (unit < 0x80) {
        buffer[0] = (char)unit;
        return 1;
    }
    if (unit < 0x800) {
        buffer[0] = (char)(0xC0 | (unit >> 6));
        buffer[1] = (char)(0x80 | (unit & 0x3F));
        return 2;
    }
    buffer[0] = (char)(0xE0 | (unit >> 12));
    buffer[1] = (char)(0x80 | ((unit >> 6) & 0x3F));
    buffer[2] = (char)(0x80 | (unit & 0x3F));
    return 3;
}

/* Render any value as bytes the caller may read but must not free. */
static const char *kt_render(KRef value, kt_int *byte_length) {
    if (value == NULL) {
        *byte_length = 4;
        return "null";
    }
    switch (value->tag) {
    case KT_STRING:
        *byte_length = value->as.string.byte_length;
        return value->as.string.bytes;
    case KT_BOOLEAN:
        if (value->as.boolean_value) {
            *byte_length = 4;
            return "true";
        }
        *byte_length = 5;
        return "false";
    case KT_UNIT:
        *byte_length = 11;
        return "kotlin.Unit";
    case KT_CHAR: {
        char *buffer = (char *)kt_alloc(4);
        *byte_length = kt_render_char(value->as.char_value, buffer);
        return buffer;
    }
    default: {
        kt_long number = 0;
        switch (value->tag) {
        case KT_BYTE:
            number = value->as.byte_value;
            break;
        case KT_SHORT:
            number = value->as.short_value;
            break;
        case KT_INT:
            number = value->as.int_value;
            break;
        default:
            number = value->as.long_value;
            break;
        }
        char *buffer = (char *)kt_alloc(24);
        *byte_length = kt_render_long(number, buffer);
        return buffer;
    }
    }
}

KRef kt_to_string(KRef value) {
    if (value != NULL && value->tag == KT_STRING) {
        return value;
    }
    kt_int length = 0;
    const char *bytes = kt_render(value, &length);
    return kt_string_of(bytes, length);
}

KRef kt_string_plus(KRef a, KRef b) {
    kt_int left_length = 0;
    kt_int right_length = 0;
    const char *left = kt_render(a, &left_length);
    const char *right = kt_render(b, &right_length);
    char *joined = (char *)kt_alloc((size_t)left_length + (size_t)right_length + 1);
    memcpy(joined, left, (size_t)left_length);
    memcpy(joined + left_length, right, (size_t)right_length);
    joined[left_length + right_length] = '\0';
    return kt_string_of(joined, left_length + right_length);
}

#define KT_BOX(suffix, tag, field, type)                                                           \
    KRef kt_box_##suffix(type value) {                                                             \
        KRef object = kt_new(tag);                                                                 \
        object->as.field = value;                                                                  \
        return object;                                                                             \
    }

KT_BOX(byte, KT_BYTE, byte_value, kt_byte)
KT_BOX(short, KT_SHORT, short_value, kt_short)
KT_BOX(int, KT_INT, int_value, kt_int)
KT_BOX(long, KT_LONG, long_value, kt_long)
KT_BOX(char, KT_CHAR, char_value, kt_char)
KT_BOX(boolean, KT_BOOLEAN, boolean_value, kt_boolean)

#undef KT_BOX

/* Unboxing a `null` is Kotlin's `NullPointerException`. With no exception machinery yet, the
   honest realization is a diagnosable exit rather than a silent zero. */
#define KT_UNBOX(suffix, field, type)                                                              \
    type kt_unbox_##suffix(KRef value) {                                                           \
        if (value == NULL) {                                                                       \
            KT_FAIL("krusty: null cannot be cast to a non-null type\n");                           \
        }                                                                                          \
        return value->as.field;                                                                    \
    }

KT_UNBOX(byte, byte_value, kt_byte)
KT_UNBOX(short, short_value, kt_short)
KT_UNBOX(int, int_value, kt_int)
KT_UNBOX(long, long_value, kt_long)
KT_UNBOX(char, char_value, kt_char)
KT_UNBOX(boolean, boolean_value, kt_boolean)

#undef KT_UNBOX

KRef kt_unit(void) {
    static KObject unit = {KT_UNIT, {{NULL, 0}}};
    return &unit;
}

/* ---- arithmetic ---------------------------------------------------------------------------- */

static void kt_divide_by_zero(void) { KT_FAIL("krusty: / by zero\n"); }

kt_int kt_div_int(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    /* INT32_MIN / -1 overflows. Kotlin wraps to INT32_MIN; C leaves it undefined. */
    if (b == -1) {
        return (kt_int)(0u - (uint32_t)a);
    }
    return a / b;
}

kt_int kt_rem_int(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    if (b == -1) {
        return 0;
    }
    return a % b;
}

kt_long kt_div_long(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    if (b == -1) {
        return (kt_long)(0u - (uint64_t)a);
    }
    return a / b;
}

kt_long kt_rem_long(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    if (b == -1) {
        return 0;
    }
    return a % b;
}

/* Kotlin masks the shift count, so `1 shl 32` is `1`, not undefined. A right shift of a negative
   value is implementation-defined in C, so the arithmetic shift is spelled out instead of assumed. */
kt_int kt_shl_int(kt_int a, kt_int bits) { return (kt_int)((uint32_t)a << (bits & 31)); }

kt_int kt_shr_int(kt_int a, kt_int bits) {
    uint32_t count = (uint32_t)(bits & 31);
    uint32_t shifted = (uint32_t)a >> count;
    if (a < 0 && count != 0) {
        shifted |= ~0u << (32 - count);
    }
    return (kt_int)shifted;
}

kt_int kt_ushr_int(kt_int a, kt_int bits) { return (kt_int)((uint32_t)a >> (bits & 31)); }

kt_long kt_shl_long(kt_long a, kt_int bits) { return (kt_long)((uint64_t)a << (bits & 63)); }

kt_long kt_shr_long(kt_long a, kt_int bits) {
    uint32_t count = (uint32_t)(bits & 63);
    uint64_t shifted = (uint64_t)a >> count;
    if (a < 0 && count != 0) {
        shifted |= ~(uint64_t)0 << (64 - count);
    }
    return (kt_long)shifted;
}

kt_long kt_ushr_long(kt_long a, kt_int bits) { return (kt_long)((uint64_t)a >> (bits & 63)); }

#define KT_COMPARE(suffix, type)                                                                   \
    kt_int kt_compare_##suffix(type a, type b) { return a < b ? -1 : (a > b ? 1 : 0); }

KT_COMPARE(byte, kt_byte)
KT_COMPARE(short, kt_short)
KT_COMPARE(int, kt_int)
KT_COMPARE(long, kt_long)
KT_COMPARE(char, kt_char)
KT_COMPARE(boolean, kt_boolean)

#undef KT_COMPARE

/* Kotlin orders floating-point values totally, which `<` and `>` do not: every NaN compares
   greater than everything including itself, and -0.0 compares below 0.0. Falling back to the bit
   patterns after the ordinary comparisons reproduces that exactly, because IEEE-754 bits of
   like-signed values are monotonic. */
kt_int kt_compare_float(kt_float a, kt_float b) {
    if (a < b) {
        return -1;
    }
    if (a > b) {
        return 1;
    }
    int32_t left = 0;
    int32_t right = 0;
    memcpy(&left, &a, sizeof left);
    memcpy(&right, &b, sizeof right);
    if (a != a) {
        left = 0x7FC00000;
    }
    if (b != b) {
        right = 0x7FC00000;
    }
    return left == right ? 0 : (left < right ? -1 : 1);
}

kt_int kt_compare_double(kt_double a, kt_double b) {
    if (a < b) {
        return -1;
    }
    if (a > b) {
        return 1;
    }
    int64_t left = 0;
    int64_t right = 0;
    memcpy(&left, &a, sizeof left);
    memcpy(&right, &b, sizeof right);
    if (a != a) {
        left = 0x7FF8000000000000LL;
    }
    if (b != b) {
        right = 0x7FF8000000000000LL;
    }
    return left == right ? 0 : (left < right ? -1 : 1);
}

/* ---- kotlin.io ----------------------------------------------------------------------------- */

static void kt_emit(KRef value, bool newline) {
    kt_int length = 0;
    const char *bytes = kt_render(value, &length);
    kt_write(1, bytes, (size_t)length);
    if (newline) {
        kt_write(1, "\n", 1);
    }
}

#define KT_CONSOLE(suffix, type, boxer)                                                            \
    void kt_print_##suffix(type value) { kt_emit(boxer(value), false); }                           \
    void kt_println_##suffix(type value) { kt_emit(boxer(value), true); }

static KRef kt_identity(KRef value) { return value; }

KT_CONSOLE(any, KRef, kt_identity)
KT_CONSOLE(byte, kt_byte, kt_box_byte)
KT_CONSOLE(short, kt_short, kt_box_short)
KT_CONSOLE(int, kt_int, kt_box_int)
KT_CONSOLE(long, kt_long, kt_box_long)
KT_CONSOLE(char, kt_char, kt_box_char)
KT_CONSOLE(boolean, kt_boolean, kt_box_boolean)

#undef KT_CONSOLE

void kt_println_unit(void) { kt_write(1, "\n", 1); }
"#;

/// The process entry point, per architecture.
///
/// With no C library there is no `crt1.o` to set up a stack frame and call `main`, so the runtime
/// supplies `_start` itself. It has to be assembly: at `_start` the stack pointer is aligned to 16
/// and points at `argc`, whereas a compiled C function's prologue assumes it was CALLED — off by
/// the width of a return address — and the mismatch shows up later as a misaligned vector spill.
pub const START: &str = r#"/* krusty native runtime — generated; do not edit. */
#include "krusty_rt.h"

void kt_program_entry(void);

#if defined(__x86_64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  xorl %ebp, %ebp\n"
        "  andq $-16, %rsp\n"
        "  call kt_program_entry\n"
        "  hlt\n");
#elif defined(__aarch64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  mov x29, #0\n"
        "  mov x30, #0\n"
        "  bl kt_program_entry\n"
        "  brk #0\n");
#elif defined(__riscv) && __riscv_xlen == 64
__asm__(".globl _start\n"
        "_start:\n"
        "  li s0, 0\n"
        "  li ra, 0\n"
        "  call kt_program_entry\n"
        "  ebreak\n");
#else
#error "krusty native: unsupported architecture"
#endif
"#;
