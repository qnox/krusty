/* krusty's stdarg.h: the compiler-provided header (C11 7.16). glibc includes it with
   __need___va_list defined to get only __gnuc_va_list. */
#ifndef __GNUC_VA_LIST
#define __GNUC_VA_LIST
typedef __builtin_va_list __gnuc_va_list;
#endif

#ifdef __need___va_list
#undef __need___va_list
#else
#ifndef __STDARG_H
#define __STDARG_H
#ifndef _VA_LIST
#define _VA_LIST
typedef __builtin_va_list va_list;
#endif
#define va_start(ap, param) __builtin_va_start(ap, param)
#define va_end(ap) __builtin_va_end(ap)
#define va_arg(ap, type) __builtin_va_arg(ap, type)
#define va_copy(dest, src) __builtin_va_copy(dest, src)
#define __va_copy(d, s) __builtin_va_copy(d, s)
#endif
#endif
