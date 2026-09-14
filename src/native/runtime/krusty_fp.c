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
 * here means a fixed array of 32-bit limbs with the three operations the algorithm needs. There is
 * no floating-point arithmetic anywhere below: a shortest-digits routine that rounded would be
 * deciding the answer with the same imprecision it exists to describe.
 *
 * Verified against the JVM's own `Double.toString`/`Float.toString` over a million values —
 * uniform random bit patterns, every small subnormal, powers of ten and their neighbours, and short
 * decimal literals — with no disagreement.
 */

#include "krusty_rt.h"

#define KT_BIG_LIMBS 72
typedef struct { uint32_t limb[KT_BIG_LIMBS]; int used; } KBig;

static void big_trim(KBig *b) { while (b->used > 0 && b->limb[b->used - 1] == 0) b->used--; }

static void big_set(KBig *b, uint64_t value) {
    memset(b->limb, 0, sizeof b->limb);
    b->limb[0] = (uint32_t)value;
    b->limb[1] = (uint32_t)(value >> 32);
    b->used = 2;
    big_trim(b);
}

/* Every operation below walks only the limbs in use. The array is sized for the widest
   intermediate this algorithm reaches, which is far wider than a typical value needs, so bounding
   the loops by `used` is most of what keeps rendering a number from costing a fixed price. */
static void big_shl(KBig *b, int bits) {
    int words = bits >> 5, rest = bits & 31;
    int top = b->used + words + 1;
    if (top > KT_BIG_LIMBS) top = KT_BIG_LIMBS;
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
    while (carry != 0 && used < KT_BIG_LIMBS) {
        b->limb[used++] = (uint32_t)carry;
        carry >>= 32;
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
    if (used < KT_BIG_LIMBS) used++;
    uint64_t carry = 0;
    for (int i = 0; i < used; i++) {
        uint64_t left = i < a->used ? a->limb[i] : 0;
        uint64_t right = i < b->used ? b->limb[i] : 0;
        uint64_t v = left + right + carry;
        dst->limb[i] = (uint32_t)v;
        carry = v >> 32;
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
   `R`, `S`, `mp` and `mm`. Digits come out most significant first and generation stops as soon as
   what has been emitted reads back as exactly this value; `min_digits` forces it to keep going,
   which is what the two-digit candidate below needs. Returns the digit count; `digits` gets the
   characters and `*k` the exponent, with value == 0.<digits> x 10^k. `*R0` and `*S0` receive the
   state after the scale fix-up, so a caller can compare candidates against the exact value. */
static int kt_shortest(uint64_t f, int e, uint64_t min_significand, int min_exponent,
                       int min_digits, char *digits, int *k, int *k0, KBig *R0, KBig *S0) {
    KBig R, S, mp, mm, t;
    int even = (f & 1) == 0;
    if (e >= 0) {
        if (f != min_significand) {
            big_set(&R, f); big_shl(&R, e + 1);
            big_set(&S, 2);
            big_set(&mp, 1); big_shl(&mp, e);
            big_set(&mm, 1); big_shl(&mm, e);
        } else {
            big_set(&R, f); big_shl(&R, e + 2);
            big_set(&S, 4);
            big_set(&mp, 1); big_shl(&mp, e + 1);
            big_set(&mm, 1); big_shl(&mm, e);
        }
    } else {
        if (e == min_exponent || f != min_significand) {
            big_set(&R, f); big_shl(&R, 1);
            big_set(&S, 1); big_shl(&S, 1 - e);
            big_set(&mp, 1);
            big_set(&mm, 1);
        } else {
            big_set(&R, f); big_shl(&R, 2);
            big_set(&S, 1); big_shl(&S, 2 - e);
            big_set(&mp, 2);
            big_set(&mm, 1);
        }
    }
    int exponent = 0;
    for (;;) {
        big_add(&t, &R, &mp);
        int c = big_cmp(&t, &S);
        if (even ? (c >= 0) : (c > 0)) { big_mul_small(&S, 10); exponent++; } else break;
    }
    for (;;) {
        big_add(&t, &R, &mp);
        big_mul_small(&t, 10);
        int c = big_cmp(&t, &S);
        if (even ? (c <= 0) : (c < 0)) {
            big_mul_small(&R, 10); big_mul_small(&mp, 10); big_mul_small(&mm, 10); exponent--;
        } else break;
    }
    if (R0 != NULL) *R0 = R;
    if (S0 != NULL) *S0 = S;
    if (k0 != NULL) *k0 = exponent;
    int n = 0;
    for (;;) {
        big_mul_small(&R, 10); big_mul_small(&mp, 10); big_mul_small(&mm, 10);
        int d = 0;
        while (big_cmp(&R, &S) >= 0) { big_sub(&R, &S); d++; }
        /* The scale fix-up places the point against the value's upper BOUNDARY, so a value sitting
           just under a power of ten — which is every second-smallest subnormal — can lead with a
           zero. A leading zero carries no information: dropping it and moving the exponent leaves
           the same number, and it is what makes `min_digits` count SIGNIFICANT digits. */
        if (n == 0 && d == 0) { exponent--; continue; }
        int lo = even ? (big_cmp(&R, &mm) <= 0) : (big_cmp(&R, &mm) < 0);
        big_add(&t, &R, &mp);
        int c = big_cmp(&t, &S);
        int hi = even ? (c >= 0) : (c > 0);
        if (n + 1 < min_digits || (!lo && !hi)) { digits[n++] = (char)('0' + d); continue; }
        if (lo && !hi) { digits[n++] = (char)('0' + d); break; }
        if (hi && !lo) { digits[n++] = (char)('0' + d + 1); break; }
        /* Both boundaries are in reach, so the nearer decimal wins and an exact tie goes to the
           even digit — the same round-half-even the hardware rounds its arithmetic with. */
        t = R; big_shl(&t, 1);
        int nearer = big_cmp(&t, &S);
        digits[n++] = (char)('0' + (nearer > 0 ? d + 1 : (nearer < 0 ? d : (d & 1 ? d + 1 : d))));
        break;
    }
    /* A rounded-up last digit can carry. The fix-up above makes a carry off the front unreachable,
       but carrying is cheaper than depending on that. */
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
    KBig R0, S0;
    int scale = 0;
    int n = kt_shortest(f, e, min_significand, min_exponent, 1, digits, k, &scale, &R0, &S0);
    if (n > 1) {
        return n;
    }
    char longer[32];
    int k2 = 0;
    int n2 = kt_shortest(f, e, min_significand, min_exponent, 2, longer, &k2, NULL, NULL, NULL);
    if (n2 != 2) {
        return n;
    }
    /* v is (R0/S0) x 10^scale, the one-digit candidate is a x 10^(k-1) and the two-digit one is
       b x 10^(k2-2). Rounding the last digit up can carry into a new leading digit and move an
       exponent, so the two are brought onto the smaller of their two powers of ten before being
       compared; from there, |X - A| against |X - B| compares their distances to v exactly, with no
       division anywhere. */
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
    KBig value = R0, from_one = S0, from_two = S0;
    for (int i = 0; i < scale - common; i++) big_mul_small(&value, 10);
    big_mul_small(&from_one, a);
    big_mul_small(&from_two, b);
    KBig gap_one = value, gap_two = value;
    if (big_cmp(&gap_one, &from_one) < 0) { gap_one = from_one; big_sub(&gap_one, &value); }
    else { big_sub(&gap_one, &from_one); }
    if (big_cmp(&gap_two, &from_two) < 0) { gap_two = from_two; big_sub(&gap_two, &value); }
    else { big_sub(&gap_two, &from_two); }
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

kt_int kt_render_double(kt_double value, char *out) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    int negative = (int)(bits >> 63);
    int biased = (int)((bits >> 52) & 0x7FF);
    uint64_t mantissa = bits & 0xFFFFFFFFFFFFFull;
    if (biased == 0x7FF) {
        if (mantissa) { memcpy(out, "NaN", 3); return 3; }
        if (negative) { memcpy(out, "-Infinity", 9); return 9; }
        memcpy(out, "Infinity", 8); return 8;
    }
    if (biased == 0 && mantissa == 0) {
        if (negative) { memcpy(out, "-0.0", 4); return 4; }
        memcpy(out, "0.0", 3); return 3;
    }
    uint64_t f = biased == 0 ? mantissa : (mantissa | (1ull << 52));
    int e = (biased == 0 ? 1 : biased) - 1023 - 52;
    char digits[32];
    int k = 0;
    int n = kt_digits(f, e, 1ull << 52, -1074, digits, &k);
    return (kt_int)kt_format(digits, n, k, negative, out);
}

kt_int kt_render_float(kt_float value, char *out) {
    uint32_t bits;
    memcpy(&bits, &value, sizeof bits);
    int negative = (int)(bits >> 31);
    int biased = (int)((bits >> 23) & 0xFF);
    uint32_t mantissa = bits & 0x7FFFFF;
    if (biased == 0xFF) {
        if (mantissa) { memcpy(out, "NaN", 3); return 3; }
        if (negative) { memcpy(out, "-Infinity", 9); return 9; }
        memcpy(out, "Infinity", 8); return 8;
    }
    if (biased == 0 && mantissa == 0) {
        if (negative) { memcpy(out, "-0.0", 4); return 4; }
        memcpy(out, "0.0", 3); return 3;
    }
    uint64_t f = biased == 0 ? mantissa : (mantissa | (1u << 23));
    int e = (biased == 0 ? 1 : biased) - 127 - 23;
    char digits[32];
    int k = 0;
    int n = kt_digits(f, e, 1ull << 23, -149, digits, &k);
    return (kt_int)kt_format(digits, n, k, negative, out);
}
