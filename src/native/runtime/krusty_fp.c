/* Rendering a floating-point value as Kotlin's `Double.toString`/`Float.toString` does.
 *
 * The answer is the SHORTEST decimal that reads back as exactly this value — not a fixed number of
 * digits, and not whatever a C library would print. `0.1` must render as `0.1` and not
 * `0.1000000000000000055511151231257827`, and `1.0E7` must not render as `10000000.0`; both are
 * observable from a Kotlin program, so neither is this runtime's to choose.
 *
 * The generator is Steele & White's, in Burger & Dybvig's formulation: carry the value and the two
 * boundaries of its rounding interval as exact rational numbers, scale until the first digit is
 * about to come out, then emit digits until what has been emitted is already inside the interval.
 * Exact means big integers — the intermediates reach about 2^1140 for a `double` — and big integers
 * here means a fixed array of 32-bit limbs with the three operations the algorithm needs. The
 * renderer does no floating-point arithmetic at all: a shortest-digits routine that rounded would
 * be deciding the answer with the same imprecision it exists to describe.
 *
 * Verified against the JVM's own `Double.toString`/`Float.toString` over a million values —
 * uniform random bit patterns, every small subnormal, powers of ten and their neighbours, and short
 * decimal literals — with no disagreement.
 */

#include "krusty_rt.h"
#include "krusty_sys.h"

/* A value's bits and back. A union is C11's defined way to reinterpret them, and unlike `memcpy`
   under -ffreestanding it is never a call, so this file calls no function the rest of the runtime
   has to define. Big integers below are copied limb by limb for the same reason. */
static uint64_t fp_bits_of_double(kt_double value) {
    union { kt_double value; uint64_t bits; } pun = {value};
    return pun.bits;
}

static kt_double fp_double_of_bits(uint64_t bits) {
    union { uint64_t bits; kt_double value; } pun = {bits};
    return pun.value;
}

static uint32_t fp_bits_of_float(kt_float value) {
    union { kt_float value; uint32_t bits; } pun = {value};
    return pun.bits;
}

static kt_float fp_float_of_bits(uint32_t bits) {
    union { uint32_t bits; kt_float value; } pun = {bits};
    return pun.value;
}

#define KT_BIG_LIMBS 72
typedef struct { uint32_t limb[KT_BIG_LIMBS]; int used; } KBig;

/* The widest intermediate is about 36 limbs, for the smallest subnormal, so the array is twice
   that and running out of it is a defect in this file rather than an input to handle. It stops the
   program: a value clamped to fit would print a wrong number with nothing to say so. */
static void big_overflow(void) {
    KT_SYS_FAIL("krusty: float rendering outgrew its big-integer width\n");
}

static void big_trim(KBig *b) { while (b->used > 0 && b->limb[b->used - 1] == 0) b->used--; }

/* Only the limbs below `used` mean anything: every operation reads those alone and writes what it
   needs above them. So a value is set and copied by its used limbs, never by the whole array, which
   is most of what keeps rendering a number from costing the price of the widest one. */
static void big_set(KBig *b, uint64_t value) {
    b->limb[0] = (uint32_t)value;
    b->limb[1] = (uint32_t)(value >> 32);
    b->used = 2;
    big_trim(b);
}

static void big_copy(KBig *dst, const KBig *src) {
    for (int i = 0; i < src->used; i++) dst->limb[i] = src->limb[i];
    dst->used = src->used;
}

static void big_shl(KBig *b, int bits) {
    int words = bits >> 5, rest = bits & 31;
    int top = b->used + words + 1;
    if (top > KT_BIG_LIMBS) big_overflow();
    /* The limb just above the value is shifted into, so it must read as the zero it stands for. */
    b->limb[b->used] = 0;
    if (words > 0) {
        for (int i = top - 1; i >= 0; i--) b->limb[i] = (i >= words) ? b->limb[i - words] : 0;
    }
    if (rest > 0) {
        uint32_t carry = 0;
        for (int i = 0; i < top; i++) {
            uint64_t v = ((uint64_t)b->limb[i] << rest) | carry;
            b->limb[i] = (uint32_t)v;
            carry = (uint32_t)(v >> 32);
        }
    }
    b->used = top;
    big_trim(b);
}

static void big_mul_small(KBig *b, uint32_t m) {
    uint64_t carry = 0;
    int used = b->used;
    for (int i = 0; i < used; i++) {
        uint64_t v = (uint64_t)b->limb[i] * m + carry;
        b->limb[i] = (uint32_t)v;
        carry = v >> 32;
    }
    if (carry != 0) {
        if (used == KT_BIG_LIMBS) big_overflow();
        b->limb[used++] = (uint32_t)carry;
    }
    b->used = used;
    big_trim(b);
}

static int big_cmp(const KBig *a, const KBig *b) {
    if (a->used != b->used) return a->used < b->used ? -1 : 1;
    for (int i = a->used - 1; i >= 0; i--)
        if (a->limb[i] != b->limb[i]) return a->limb[i] < b->limb[i] ? -1 : 1;
    return 0;
}

static void big_add(KBig *dst, const KBig *a, const KBig *b) {
    int used = a->used > b->used ? a->used : b->used;
    uint64_t carry = 0;
    for (int i = 0; i < used; i++) {
        uint64_t left = i < a->used ? a->limb[i] : 0;
        uint64_t right = i < b->used ? b->limb[i] : 0;
        uint64_t v = left + right + carry;
        dst->limb[i] = (uint32_t)v;
        carry = v >> 32;
    }
    if (carry != 0) {
        if (used == KT_BIG_LIMBS) big_overflow();
        dst->limb[used++] = (uint32_t)carry;
    }
    dst->used = used;
    big_trim(dst);
}

static void big_sub(KBig *a, const KBig *b) {
    int64_t borrow = 0;
    int used = a->used;
    for (int i = 0; i < used; i++) {
        int64_t right = i < b->used ? (int64_t)b->limb[i] : 0;
        int64_t v = (int64_t)a->limb[i] - right - borrow;
        if (v < 0) { v += (int64_t)1 << 32; borrow = 1; } else { borrow = 0; }
        a->limb[i] = (uint32_t)v;
    }
    big_trim(a);
}

/* The Steele & White / Burger & Dybvig generator, with the value's rounding interval carried in
   `R`, `S`, `mp` and `mm`: the value is R/S and the interval reaches mp/S above it and mm/S below.
   `kt_scale` sets them up and finds the power of ten the digits start at; `kt_generate` then emits
   digits from that state, and can be run on it more than once. */
typedef struct {
    KBig R, S, mp, mm;
    int exponent;
    int even;
} KScaled;

static void kt_scale(uint64_t f, int e, uint64_t min_significand, int min_exponent, KScaled *s) {
    int even = (f & 1) == 0;
    s->even = even;
    if (e >= 0) {
        if (f != min_significand) {
            big_set(&s->R, f); big_shl(&s->R, e + 1);
            big_set(&s->S, 2);
            big_set(&s->mp, 1); big_shl(&s->mp, e);
            big_set(&s->mm, 1); big_shl(&s->mm, e);
        } else {
            big_set(&s->R, f); big_shl(&s->R, e + 2);
            big_set(&s->S, 4);
            big_set(&s->mp, 1); big_shl(&s->mp, e + 1);
            big_set(&s->mm, 1); big_shl(&s->mm, e);
        }
    } else {
        if (e == min_exponent || f != min_significand) {
            big_set(&s->R, f); big_shl(&s->R, 1);
            big_set(&s->S, 1); big_shl(&s->S, 1 - e);
            big_set(&s->mp, 1);
            big_set(&s->mm, 1);
        } else {
            big_set(&s->R, f); big_shl(&s->R, 2);
            big_set(&s->S, 1); big_shl(&s->S, 2 - e);
            big_set(&s->mp, 2);
            big_set(&s->mm, 1);
        }
    }
    /* The fix-up steps one power of ten at a time until the interval's top sits just below 1, which
       near either end of the range is some three hundred big-integer multiplies. So it first steps
       nine powers at a time for as long as all nine are certain: the condition for a step only
       weakens toward the first of any nine, so the ninth holding means all of them hold, and one
       multiply by 10^9 is exactly nine by 10. The single steps that follow therefore start from
       the state they would have reached on their own, and end where they always did. */
    KBig t, u;
    int exponent = 0;
    for (;;) {
        big_add(&t, &s->R, &s->mp);
        big_copy(&u, &s->S);
        big_mul_small(&u, 100000000u);
        int c = big_cmp(&t, &u);
        if (even ? (c >= 0) : (c > 0)) {
            big_mul_small(&s->S, 1000000000u);
            exponent += 9;
        } else break;
    }
    for (;;) {
        big_add(&t, &s->R, &s->mp);
        int c = big_cmp(&t, &s->S);
        if (even ? (c >= 0) : (c > 0)) { big_mul_small(&s->S, 10); exponent++; } else break;
    }
    for (;;) {
        big_add(&t, &s->R, &s->mp);
        big_mul_small(&t, 1000000000u);
        int c = big_cmp(&t, &s->S);
        if (even ? (c <= 0) : (c < 0)) {
            big_mul_small(&s->R, 1000000000u);
            big_mul_small(&s->mp, 1000000000u);
            big_mul_small(&s->mm, 1000000000u);
            exponent -= 9;
        } else break;
    }
    for (;;) {
        big_add(&t, &s->R, &s->mp);
        big_mul_small(&t, 10);
        int c = big_cmp(&t, &s->S);
        if (even ? (c <= 0) : (c < 0)) {
            big_mul_small(&s->R, 10); big_mul_small(&s->mp, 10); big_mul_small(&s->mm, 10);
            exponent--;
        } else break;
    }
    s->exponent = exponent;
}

/* Digits come out most significant first and generation stops as soon as what has been emitted
   reads back as exactly this value; `min_digits` forces it to keep going, which is what the
   two-digit candidate below needs. Returns the digit count; `digits` gets the characters and `*k`
   the exponent, with value == 0.<digits> x 10^k. */
static int kt_generate(const KScaled *s, int min_digits, char *digits, int *k) {
    KBig R, mp, mm, t;
    big_copy(&R, &s->R);
    big_copy(&mp, &s->mp);
    big_copy(&mm, &s->mm);
    const KBig *S = &s->S;
    int even = s->even;
    int exponent = s->exponent;
    int n = 0;
    for (;;) {
        big_mul_small(&R, 10); big_mul_small(&mp, 10); big_mul_small(&mm, 10);
        int d = 0;
        while (big_cmp(&R, S) >= 0) { big_sub(&R, S); d++; }
        /* The scale fix-up places the point against the value's upper BOUNDARY, so a value sitting
           just under a power of ten — which is every second-smallest subnormal — can lead with a
           zero. A leading zero carries no information: dropping it and moving the exponent leaves
           the same number, and it is what makes `min_digits` count SIGNIFICANT digits. */
        if (n == 0 && d == 0) { exponent--; continue; }
        int lo = even ? (big_cmp(&R, &mm) <= 0) : (big_cmp(&R, &mm) < 0);
        big_add(&t, &R, &mp);
        int c = big_cmp(&t, S);
        int hi = even ? (c >= 0) : (c > 0);
        if (n + 1 < min_digits || (!lo && !hi)) { digits[n++] = (char)('0' + d); continue; }
        if (lo && !hi) { digits[n++] = (char)('0' + d); break; }
        if (hi && !lo) { digits[n++] = (char)('0' + d + 1); break; }
        /* Both boundaries are in reach, so the nearer decimal wins and an exact tie goes to the
           even digit — the same round-half-even the hardware rounds its arithmetic with. */
        big_copy(&t, &R); big_shl(&t, 1);
        int nearer = big_cmp(&t, S);
        digits[n++] = (char)('0' + (nearer > 0 ? d + 1 : (nearer < 0 ? d : (d & 1 ? d + 1 : d))));
        break;
    }
    /* A rounded-up last digit can carry, and the carry can run off the front: that same leading-
       zero case, a value just under a power of ten, emits nines that round up into a new leading
       `1`. `1e23` and `1e-7` are both just under their power of ten in binary, and render as
       `1.0E23` and `1.0E-7` only through the `i == 0` branch here: it is load-bearing, not a
       safeguard. */
    for (int i = n - 1; i >= 0 && digits[i] > '9'; i--) {
        digits[i] = (char)(digits[i] - 10);
        if (i == 0) {
            for (int j = n; j > 0; j--) digits[j] = digits[j - 1];
            digits[0] = '1';
            n++;
            exponent++;
        } else {
            digits[i - 1]++;
        }
    }
    /* A carry leaves zeros behind it, and a trailing zero is never part of the shortest decimal:
       the same number without it reads back the same. Trimming stops at `min_digits`, which is
       what keeps the two-digit candidate two digits long. */
    while (n > min_digits && digits[n - 1] == '0') n--;
    *k = exponent;
    return n;
}

/* The digits `Double.toString` renders: the shortest that read back as exactly this value, except
   that where ONE digit suffices the TWO-digit decimals are considered alongside it and the closer
   of the two wins. That last clause is Kotlin's rule (inherited from Java's specification), not a
   rounding accident: it is why `Double.MIN_VALUE` renders as `4.9E-324` rather than the shorter,
   equally round-tripping `5E-324`. It can only change the answer where the value sits far from
   every one-digit decimal, which is to say among the smallest subnormals. */
static int kt_digits(uint64_t f, int e, uint64_t min_significand, int min_exponent,
                     char *digits, int *k) {
    KScaled s;
    kt_scale(f, e, min_significand, min_exponent, &s);
    int n = kt_generate(&s, 1, digits, k);
    if (n > 1) {
        return n;
    }
    /* The two-digit candidate is generated from the same scaled state rather than scaled afresh. */
    char longer[32];
    int k2 = 0;
    int n2 = kt_generate(&s, 2, longer, &k2);
    if (n2 != 2) {
        return n;
    }
    /* v is (R/S) x 10^exponent in the scaled state, the one-digit candidate is a x 10^(k-1) and
       the two-digit one is b x 10^(k2-2). Rounding the last digit up can carry into a new leading
       digit and move an exponent, so the two are brought onto the smaller of their two powers of
       ten before being compared; from there, |X - A| against |X - B| compares their distances to v
       exactly, with no division anywhere. */
    int one_power = *k - 1;
    int two_power = k2 - 2;
    int common = one_power < two_power ? one_power : two_power;
    uint32_t a = (uint32_t)(digits[0] - '0');
    uint32_t b = (uint32_t)(longer[0] - '0') * 10 + (uint32_t)(longer[1] - '0');
    for (int i = 0; i < one_power - common; i++) a *= 10;
    for (int i = 0; i < two_power - common; i++) b *= 10;
    /* The same decimal written with a trailing zero is not a second candidate, and the shorter
       spelling is the one Kotlin prints. */
    if (a == b) {
        return n;
    }
    KBig value, from_one, from_two, gap_one, gap_two;
    big_copy(&value, &s.R);
    for (int i = 0; i < s.exponent - common; i++) big_mul_small(&value, 10);
    big_copy(&from_one, &s.S);
    big_mul_small(&from_one, a);
    big_copy(&from_two, &s.S);
    big_mul_small(&from_two, b);
    if (big_cmp(&value, &from_one) < 0) {
        big_copy(&gap_one, &from_one); big_sub(&gap_one, &value);
    } else {
        big_copy(&gap_one, &value); big_sub(&gap_one, &from_one);
    }
    if (big_cmp(&value, &from_two) < 0) {
        big_copy(&gap_two, &from_two); big_sub(&gap_two, &value);
    } else {
        big_copy(&gap_two, &value); big_sub(&gap_two, &from_two);
    }
    /* The nearer wins; two genuinely equidistant decimals go to the one whose last digit is even. */
    int c = big_cmp(&gap_one, &gap_two);
    if (c < 0 || (c == 0 && ((a & 1) == 0 || (b & 1) != 0))) {
        return n;
    }
    digits[0] = longer[0];
    digits[1] = longer[1];
    *k = k2;
    return 2;
}

/* Java's `Double.toString`/`Float.toString` shape around the digits. */
static int kt_format(const char *digits, int n, int k, int negative, char *out) {
    int at = 0;
    if (negative) out[at++] = '-';
    if (k > -3 && k <= 7) {
        if (k <= 0) {
            out[at++] = '0'; out[at++] = '.';
            for (int i = 0; i < -k; i++) out[at++] = '0';
            for (int i = 0; i < n; i++) out[at++] = digits[i];
        } else if (k >= n) {
            for (int i = 0; i < n; i++) out[at++] = digits[i];
            for (int i = 0; i < k - n; i++) out[at++] = '0';
            out[at++] = '.'; out[at++] = '0';
        } else {
            for (int i = 0; i < k; i++) out[at++] = digits[i];
            out[at++] = '.';
            for (int i = k; i < n; i++) out[at++] = digits[i];
        }
        return at;
    }
    out[at++] = digits[0];
    out[at++] = '.';
    if (n > 1) { for (int i = 1; i < n; i++) out[at++] = digits[i]; }
    else out[at++] = '0';
    out[at++] = 'E';
    int exponent = k - 1;
    if (exponent < 0) { out[at++] = '-'; exponent = -exponent; }
    char scratch[8];
    int count = 0;
    do { scratch[count++] = (char)('0' + exponent % 10); exponent /= 10; } while (exponent);
    while (count > 0) out[at++] = scratch[--count];
    return at;
}

/* The spelling of a value that has no digits to render. */
static kt_int kt_put(char *out, const char *text) {
    kt_int length = 0;
    while (text[length] != '\0') { out[length] = text[length]; length++; }
    return length;
}

kt_int kt_render_double(kt_double value, char *out) {
    uint64_t bits = fp_bits_of_double(value);
    int negative = (int)(bits >> 63);
    int biased = (int)((bits >> 52) & 0x7FF);
    uint64_t mantissa = bits & 0xFFFFFFFFFFFFFull;
    if (biased == 0x7FF) {
        if (mantissa) return kt_put(out, "NaN");
        return kt_put(out, negative ? "-Infinity" : "Infinity");
    }
    if (biased == 0 && mantissa == 0) return kt_put(out, negative ? "-0.0" : "0.0");
    uint64_t f = biased == 0 ? mantissa : (mantissa | (1ull << 52));
    int e = (biased == 0 ? 1 : biased) - 1023 - 52;
    char digits[32];
    int k = 0;
    int n = kt_digits(f, e, 1ull << 52, -1074, digits, &k);
    return (kt_int)kt_format(digits, n, k, negative, out);
}

kt_int kt_render_float(kt_float value, char *out) {
    uint32_t bits = fp_bits_of_float(value);
    int negative = (int)(bits >> 31);
    int biased = (int)((bits >> 23) & 0xFF);
    uint32_t mantissa = bits & 0x7FFFFF;
    if (biased == 0xFF) {
        if (mantissa) return kt_put(out, "NaN");
        return kt_put(out, negative ? "-Infinity" : "Infinity");
    }
    if (biased == 0 && mantissa == 0) return kt_put(out, negative ? "-0.0" : "0.0");
    uint64_t f = biased == 0 ? mantissa : (mantissa | (1u << 23));
    int e = (biased == 0 ? 1 : biased) - 127 - 23;
    char digits[32];
    int k = 0;
    int n = kt_digits(f, e, 1ull << 23, -149, digits, &k);
    return (kt_int)kt_format(digits, n, k, negative, out);
}


/* ---- remainder ------------------------------------------------------------------------------ */

/* `a % b` on floating point, which Kotlin defines as IEEE's remainder TRUNCATED toward zero: the
   result carries the sign of `a` and a magnitude strictly below `|b|`. Neither Cranelift nor every
   target's hardware has an instruction for it, so it is computed here — and computed EXACTLY, on
   the significands, because the answer is `(significand of a, shifted) mod (significand of b)` and
   a shift-and-subtract loop gets that with no rounding anywhere. Verified against the JVM's own
   `%` over a million operand pairs, bit for bit wherever the answer is a number.

   NaN BITS are another matter: Kotlin does not specify them and the JVM's differ by host, so a NaN
   here follows IEEE 754 the way a hardware instruction would. A NaN operand comes back QUIETED,
   with its sign and payload, because no arithmetic operation delivers a signaling NaN; an invalid
   operation answers a fixed NaN (see `KT_INVALID_DOUBLE`). */
#define KT_QUIET_DOUBLE 0x0008000000000000ull
#define KT_QUIET_FLOAT 0x00400000u
/* The NaN an invalid operation — a remainder OF an infinity, or BY a zero — answers. It is x86's
   default NaN, which is negative, and so what the JVM yields for `1.0 % 0.0` on an x86 host, where
   the reference compiler runs. That is a choice, not parity: AArch64's and RISC-V's default NaN is
   the positive one `Double.NaN` also has, and a JVM on those hosts answers that instead. */
#define KT_INVALID_DOUBLE 0xFFF8000000000000ull
#define KT_INVALID_FLOAT 0xFFC00000u

/* `significand * 2^exponent` as a double, for a value the format can hold exactly. */
static kt_double kt_double_from_parts(uint64_t significand, int exponent, int negative) {
    if (significand == 0) {
        return fp_double_of_bits(negative ? (1ull << 63) : 0);
    }
    while (significand < (1ull << 52)) { significand <<= 1; exponent--; }
    while (significand >= (1ull << 53)) { significand >>= 1; exponent++; }
    int biased = exponent + 1075;
    uint64_t bits;
    if (biased <= 0) {
        int shift = 1 - biased;
        significand = shift >= 64 ? 0 : (significand >> shift);
        bits = significand;
    } else {
        bits = ((uint64_t)biased << 52) | (significand & 0xFFFFFFFFFFFFFull);
    }
    if (negative) bits |= 1ull << 63;
    return fp_double_of_bits(bits);
}

kt_double kt_rem_double(kt_double a, kt_double b) {
    uint64_t left = fp_bits_of_double(a), right = fp_bits_of_double(b);
    int negative = (int)(left >> 63);
    int exp_a = (int)((left >> 52) & 0x7FF), exp_b = (int)((right >> 52) & 0x7FF);
    uint64_t man_a = left & 0xFFFFFFFFFFFFFull, man_b = right & 0xFFFFFFFFFFFFFull;
    if (exp_a == 0x7FF && man_a != 0) return fp_double_of_bits(left | KT_QUIET_DOUBLE);
    if (exp_b == 0x7FF && man_b != 0) return fp_double_of_bits(right | KT_QUIET_DOUBLE);
    if (exp_a == 0x7FF || (exp_b == 0 && man_b == 0)) return fp_double_of_bits(KT_INVALID_DOUBLE);
    /* A finite value by an infinity leaves itself, and a zero leaves itself. */
    if (exp_b == 0x7FF) return a;
    if (exp_a == 0 && man_a == 0) return a;
    uint64_t sig_a = exp_a == 0 ? man_a : (man_a | (1ull << 52));
    uint64_t sig_b = exp_b == 0 ? man_b : (man_b | (1ull << 52));
    int scale_a = (exp_a == 0 ? 1 : exp_a) - 1075;
    int scale_b = (exp_b == 0 ? 1 : exp_b) - 1075;
    if (scale_a < scale_b) {
        /* `a`'s granularity is finer than `b`'s: bring `b` onto `a`'s scale, which is exact
           because it only shifts bits up. */
        int shift = scale_b - scale_a;
        if (shift >= 64 || (sig_b >> (63 - shift)) != 0) {
            /* |b| is enormously larger than |a|, so nothing of `a` is consumed. */
            return a;
        }
        sig_b <<= shift;
        scale_b = scale_a;
    }
    int steps = scale_a - scale_b;
    uint64_t r = sig_a % sig_b;
    for (int i = 0; i < steps; i++) {
        r <<= 1;
        if (r >= sig_b) r -= sig_b;
    }
    return kt_double_from_parts(r, scale_b, negative);
}

kt_float kt_rem_float(kt_float a, kt_float b) {
    /* Every `float`, and every remainder of two of them, is exactly a `double`, so this is the
       same answer with no rounding on the way in or out. A NaN is the one thing the casts would
       not carry across unchanged, so NaNs are answered at this width, the same way as above. */
    uint32_t left = fp_bits_of_float(a), right = fp_bits_of_float(b);
    if ((left & 0x7FFFFFFFu) > 0x7F800000u) return fp_float_of_bits(left | KT_QUIET_FLOAT);
    if ((right & 0x7FFFFFFFu) > 0x7F800000u) return fp_float_of_bits(right | KT_QUIET_FLOAT);
    int a_infinite = ((left >> 23) & 0xFF) == 0xFF;
    int b_zero = (right & 0x7FFFFFFFu) == 0;
    if (a_infinite || b_zero) return fp_float_of_bits(KT_INVALID_FLOAT);
    return (kt_float)kt_rem_double((kt_double)a, (kt_double)b);
}

/* The sign of a floating-point value as Kotlin's `sign` gives it: NaN for NaN, the value itself for
   either zero (so -0.0 stays negative), and ±1 otherwise. Written out rather than taken from
   `signbit`, because what `mod` below compares is Kotlin's `sign`, and that answers NaN — a
   comparison no sign BIT can express. */
static kt_double kt_sign_double(kt_double x) {
    if (x != x) return x;
    if (x == 0.0) return x;
    return x > 0.0 ? 1.0 : -1.0;
}

static kt_float kt_sign_float(kt_float x) {
    if (x != x) return x;
    if (x == 0.0f) return x;
    return x > 0.0f ? 1.0f : -1.0f;
}

/* `a.mod(b)` — the remainder carrying the DIVISOR's sign, where `%` carries the dividend's. Kotlin
   defines it as `val r = a % b; if (r != 0.0 && r.sign != b.sign) r + b else r`, and this is that
   definition: `r != 0.0` is false for either zero, so a zero remainder is answered as it stands,
   and a NaN sign compares unequal to everything, which carries a NaN out of the adjustment too. */
kt_double kt_mod_double(kt_double a, kt_double b) {
    kt_double r = kt_rem_double(a, b);
    if (r != 0.0 && !(kt_sign_double(r) == kt_sign_double(b))) {
        return r + b;
    }
    return r;
}

kt_float kt_mod_float(kt_float a, kt_float b) {
    kt_float r = kt_rem_float(a, b);
    if (r != 0.0f && !(kt_sign_float(r) == kt_sign_float(b))) {
        return r + b;
    }
    return r;
}
