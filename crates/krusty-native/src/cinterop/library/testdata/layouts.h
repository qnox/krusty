#define B_HEX 0x10
#define B_LONG 10L
#define B_ULONG 10UL
#define B_LL 5LL
#define B_NEG (-1)
#define B_CHAR 'a'
#define B_STR "ab" "cd"
#define B_FLOAT 1.5f
#define B_DOUBLE 2.25
#define B_REF B_HEX
#define B_CAST ((int)5)
#define B_SIZEOF (sizeof(int))
#define B_BIG 0xFFFFFFFFu
#define B_BIGGER 0x100000000
#define B_SHIFT (1 << 31)
#define B_FN(x) ((x) + 1)
#define B_EMPTY
#define B_UNSIGNED_CHAR ((unsigned char)200)
typedef struct { int a; double b; } anon_t;
struct inner { short s; char c; };
union u { int i; float f; char bytes[4]; };
struct outer {
  struct inner in;
  struct inner *pin;
  int arr[3];
  char name[16];
  union u un;
  struct { int nx; int ny; } nested;
  union { long la; double da; };
  int (*cb)(int);
  void *opaque;
  const char *str;
  _Bool flag;
  signed char i8; unsigned char u8; short i16; unsigned short u16; int i32; unsigned int u32; long i64; unsigned long u64;
  float fl; long double ld;
  enum { O_A, O_B } kind;
  signed char sc; unsigned char uc; char pc;
  unsigned long ul; long long ll; unsigned long long ull;
  int flex[];
};
typedef enum { S_ONE, S_TWO, S_THREE } strict_t;
enum named_strict { NS_A, NS_B };
typedef int myint;
typedef myint myint2;
typedef struct outer outer_t;
typedef struct opaque_s *handle_t;
typedef void (*voidfn)(void);
extern struct inner global_inner;
extern const int const_global;
extern int global_array[10];
extern strict_t global_strict;
extern enum named_strict global_named;
struct inner make_inner(int x);
void take_ptr(struct outer *o, const struct inner *ci, anon_t a, anon_t *ap);
const char *get_name(void);
char **get_names(int *count);
void arr_param(int values[4], char *argv[]);
_Noreturn void die(const char *msg);
void noproto();
strict_t use_strict(strict_t s, enum named_strict n, myint2 m, handle_t h, voidfn f);
unsigned long widths(signed char a, unsigned char b, short c, unsigned short d, unsigned int e, _Bool f, float g, long h, unsigned long i, signed char j, unsigned char k);
void excluded_one(void);
static inline int inline_helper(int x) { return x; }
void *alloc(unsigned long size);
void with_fnptr_param(int (*cmp)(const void *, const void *));
long double ldfn(long double x);
enum color_flags { CF_RED = 1, CF_GREEN = 2 };
extern enum color_flags g_color;
struct attrs { char c; double d __attribute__((aligned(16))); short s; } __attribute__((aligned(32)));
union with_anon { struct { int a; int b; }; long l; };
struct nested_arrays { int grid[2][3]; struct inner cells[2]; };
typedef struct attrs attrs_t, *attrs_p;
#define B_STRUCT_EXPR global_inner
#define B_ARRAY_EXPR global_array
#define B_ADDR (&global_inner)
#define B_SIZE_CAST ((unsigned long)sizeof(struct outer))
#define B_TERNARY (B_HEX > 4 ? 1.5 : 2)
