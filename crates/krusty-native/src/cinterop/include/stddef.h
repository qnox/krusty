/* krusty's stddef.h: the compiler-provided header (C11 7.19), defined from the target's predefined
   macros. It honours glibc's __need_* protocol: a header that defines __need_size_t (and so on)
   before including it gets only those names. */
#if !defined(__need_size_t) && !defined(__need_ptrdiff_t) && !defined(__need_wchar_t) && \
    !defined(__need_NULL) && !defined(__need_wint_t) && !defined(__need_max_align_t) && \
    !defined(__need_offsetof)
#define __need_size_t
#define __need_ptrdiff_t
#define __need_wchar_t
#define __need_NULL
#define __need_max_align_t
#define __need_offsetof
#define __STDDEF_H
#endif

#if defined(__need_size_t) && !defined(_SIZE_T)
#define _SIZE_T
typedef __SIZE_TYPE__ size_t;
#endif
#undef __need_size_t

#if defined(__need_ptrdiff_t) && !defined(_PTRDIFF_T)
#define _PTRDIFF_T
typedef __PTRDIFF_TYPE__ ptrdiff_t;
#endif
#undef __need_ptrdiff_t

#if defined(__need_wchar_t) && !defined(_WCHAR_T)
#define _WCHAR_T
typedef __WCHAR_TYPE__ wchar_t;
#endif
#undef __need_wchar_t

#if defined(__need_wint_t) && !defined(_WINT_T)
#define _WINT_T
typedef __WINT_TYPE__ wint_t;
#endif
#undef __need_wint_t

#if defined(__need_NULL)
#undef NULL
#define NULL ((void *)0)
#endif
#undef __need_NULL

#if defined(__need_max_align_t) && !defined(__CLANG_MAX_ALIGN_T_DEFINED)
#define __CLANG_MAX_ALIGN_T_DEFINED
typedef struct {
  long long __clang_max_align_nonce1 __attribute__((__aligned__(__alignof__(long long))));
  long double __clang_max_align_nonce2 __attribute__((__aligned__(__alignof__(long double))));
} max_align_t;
#endif
#undef __need_max_align_t

#if defined(__need_offsetof)
#undef offsetof
#define offsetof(t, d) __builtin_offsetof(t, d)
#endif
#undef __need_offsetof
