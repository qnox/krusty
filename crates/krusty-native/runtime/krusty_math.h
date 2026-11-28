/* The `kotlin.math` functions Kotlin/Native declares `external` and answers from the platform's
 * libm: `exp`, `pow`, `atan` and `atan2`, over `Double` and `Float`. This runtime links no libm,
 * so it carries them.
 *
 * `exp`, `pow`, `expf` and `powf` are the routines of Arm's optimized-routines (Szabolcs Nagy),
 * which glibc 2.28 and musl both carry, so a program gets the answers a Kotlin/Native program
 * linked against either gets: results within 0.52 ulp, almost always the correctly rounded one.
 * Copyright (c) 2017-2018, Arm Limited. SPDX-License-Identifier: MIT.
 *
 * `atan` and `atan2` are fdlibm's, as FreeBSD's `msun` ships them.
 * Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved. Developed at SunPro, a Sun
 * Microsystems, Inc. business. Permission to use, copy, modify, and distribute this software is
 * freely granted, provided that this notice is preserved.
 *
 * The files `krusty_math_*.c` share this header: the word access macros, the shared tables, and
 * the error helpers those routines call.
 */
#ifndef KRUSTY_MATH_H
#define KRUSTY_MATH_H

#include "krusty_rt.h"
#include "krusty_sys.h"

#define hidden
#define issignaling_inline(x) 0
#define issignalingf_inline(x) 0
#define INFINITY (__builtin_inff())
#define NAN (__builtin_nanf(""))
#define TOINT_INTRINSICS 0
#define WANT_ROUNDING 1
#define predict_true(x) __builtin_expect(!!(x), 1)
#define predict_false(x) __builtin_expect(x, 0)
typedef double double_t;
typedef float float_t;
static inline float eval_as_float(float x) { float y = x; return y; }
static inline double eval_as_double(double x) { double y = x; return y; }
static inline float fp_barrierf(float x) { volatile float y = x; return y; }
static inline double fp_barrier(double x) { volatile double y = x; return y; }
static inline void fp_force_eval(double x) { volatile double y = x; (void)y; }
#define asuint(f) ((union{float _f; uint32_t _i;}){f})._i
#define asfloat(i) ((union{uint32_t _i; float _f;}){i})._f
#define asuint64(f) ((union{double _f; uint64_t _i;}){f})._i
#define asdouble(i) ((union{uint64_t _i; double _f;}){i})._f
#define EXTRACT_WORDS(hi, lo, d) do { uint64_t u_ = asuint64(d); (hi) = u_ >> 32; (lo) = (uint32_t)u_; } while (0)
#define GET_HIGH_WORD(hi, d) do { (hi) = asuint64(d) >> 32; } while (0)
#define GET_LOW_WORD(lo, d) do { (lo) = (uint32_t)asuint64(d); } while (0)
#define INSERT_WORDS(d, hi, lo) do { (d) = asdouble(((uint64_t)(hi) << 32) | (uint32_t)(lo)); } while (0)
#define SET_HIGH_WORD(d, hi) INSERT_WORDS(d, hi, (uint32_t)asuint64(d))
#define SET_LOW_WORD(d, lo) INSERT_WORDS(d, asuint64(d) >> 32, lo)
#define GET_FLOAT_WORD(w, d) do { (w) = asuint(d); } while (0)
#define SET_FLOAT_WORD(d, w) do { (d) = asfloat(w); } while (0)
#define STRICT_ASSIGN(type, lval, rval) ((lval) = (rval))
#define nan_mix(x, y) ((x) + (y))
#define fabs(x) __builtin_fabs(x)
#define fabsf(x) __builtin_fabsf(x)

double __math_oflow(uint32_t);
double __math_uflow(uint32_t);
double __math_invalid(double);
double __math_divzero(uint32_t);
double __math_xflow(uint32_t, double);
float __math_oflowf(uint32_t);
float __math_uflowf(uint32_t);
float __math_invalidf(float);
float __math_divzerof(uint32_t);
float __math_xflowf(uint32_t, float);

/*
 * Copyright (c) 2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


#define EXP_TABLE_BITS 7
#define EXP_POLY_ORDER 5
#define EXP_USE_TOINT_NARROW 0
#define EXP2_POLY_ORDER 5
extern const struct exp_data {
	double invln2N;
	double shift;
	double negln2hiN;
	double negln2loN;
	double poly[4]; /* Last four coefficients.  */
	double exp2_shift;
	double exp2_poly[EXP2_POLY_ORDER];
	uint64_t tab[2*(1 << EXP_TABLE_BITS)];
} __exp_data;

/*
 * Copyright (c) 2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


#define POW_LOG_TABLE_BITS 7
#define POW_LOG_POLY_ORDER 8
extern const struct pow_log_data {
	double ln2hi;
	double ln2lo;
	double poly[POW_LOG_POLY_ORDER - 1]; /* First coefficient is 1.  */
	/* Note: the pad field is unused, but allows slightly faster indexing.  */
	struct {
		double invc, pad, logc, logctail;
	} tab[1 << POW_LOG_TABLE_BITS];
} __pow_log_data;

/*
 * Copyright (c) 2017-2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


/* Shared between expf, exp2f and powf.  */
#define EXP2F_TABLE_BITS 5
#define EXP2F_POLY_ORDER 3
extern const struct exp2f_data {
	uint64_t tab[1 << EXP2F_TABLE_BITS];
	double shift_scaled;
	double poly[EXP2F_POLY_ORDER];
	double shift;
	double invln2_scaled;
	double poly_scaled[EXP2F_POLY_ORDER];
} __exp2f_data;

/*
 * Copyright (c) 2017-2018, Arm Limited.
 * SPDX-License-Identifier: MIT
 */


#define POWF_LOG2_TABLE_BITS 4
#define POWF_LOG2_POLY_ORDER 5
#if TOINT_INTRINSICS
#define POWF_SCALE_BITS EXP2F_TABLE_BITS
#else
#define POWF_SCALE_BITS 0
#endif
#define POWF_SCALE ((double)(1 << POWF_SCALE_BITS))
extern const struct powf_log2_data {
	struct {
		double invc, logc;
	} tab[1 << POWF_LOG2_TABLE_BITS];
	double poly[POWF_LOG2_POLY_ORDER];
} __powf_log2_data;

#endif
