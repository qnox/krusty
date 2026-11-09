/* krusty's limits.h (C11 7.10): the C library's, then the compiler's own limits from the target's
   predefined macros. _GCC_LIMITS_H_ tells glibc's limits.h not to look for another one. */
#ifndef __KRUSTY_LIMITS_H
#define __KRUSTY_LIMITS_H
#define _GCC_LIMITS_H_
#if __STDC_HOSTED__ && __has_include_next(<limits.h>)
#include_next <limits.h>
#endif
#undef SCHAR_MAX
#undef SCHAR_MIN
#undef UCHAR_MAX
#undef CHAR_BIT
#undef CHAR_MIN
#undef CHAR_MAX
#undef SHRT_MAX
#undef SHRT_MIN
#undef USHRT_MAX
#undef INT_MAX
#undef INT_MIN
#undef UINT_MAX
#undef LONG_MAX
#undef LONG_MIN
#undef ULONG_MAX
#undef LLONG_MAX
#undef LLONG_MIN
#undef ULLONG_MAX
#undef MB_LEN_MAX
#define SCHAR_MAX __SCHAR_MAX__
#define SHRT_MAX __SHRT_MAX__
#define INT_MAX __INT_MAX__
#define LONG_MAX __LONG_MAX__
#define LLONG_MAX __LONG_LONG_MAX__
#define SCHAR_MIN (-__SCHAR_MAX__ - 1)
#define SHRT_MIN (-__SHRT_MAX__ - 1)
#define INT_MIN (-__INT_MAX__ - 1)
#define LONG_MIN (-__LONG_MAX__ - 1L)
#define LLONG_MIN (-__LONG_LONG_MAX__ - 1LL)
#define UCHAR_MAX (__SCHAR_MAX__ * 2 + 1)
#define USHRT_MAX (__SHRT_MAX__ * 2 + 1)
#define UINT_MAX (__INT_MAX__ * 2U + 1U)
#define ULONG_MAX (__LONG_MAX__ * 2UL + 1UL)
#define ULLONG_MAX (__LONG_LONG_MAX__ * 2ULL + 1ULL)
#define MB_LEN_MAX 1
#define CHAR_BIT __CHAR_BIT__
#ifdef __CHAR_UNSIGNED__
#define CHAR_MIN 0
#define CHAR_MAX UCHAR_MAX
#else
#define CHAR_MIN SCHAR_MIN
#define CHAR_MAX __SCHAR_MAX__
#endif
#endif
