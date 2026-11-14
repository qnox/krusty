int scan_it(const char *f, ...) __asm__("real_scan_symbol");
typedef int (*vfn)(int, ...);
typedef int plain_fn(int);
enum color3 { C_RED = 1, C_BLUE = 2 };
enum strict3 { S3_A, S3_B };
struct bitsy { enum color3 c : 3; _Bool flag : 1; int s : 5; enum strict3 st : 2; };
union ubits { int a : 3; char b; };
static int static_var;
extern _Thread_local int tls_var;
static int static_decl(void);
extern plain_fn *fn_ptr_global;
void takes_plain(plain_fn *f, vfn v);
#define take_it scan_it
#define DUP 1
#define DUP 2
int MACRO_FN_CLASH(void);
#define MACRO_FN_CLASH 5
extern int var_clash;
#define var_clash 7
typedef __builtin_va_list my_va;
int format_v(const char *f, my_va ap);
int open_it(int level, const char *version);
#define open_init(level) open_it((level), "1.0")
#undef open_init
int open_init(int level);
#define gone_macro(x) (x)
#undef gone_macro
#define kept_macro(x) (x)
