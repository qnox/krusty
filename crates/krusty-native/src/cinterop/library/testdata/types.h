typedef const char *cstr;
typedef unsigned int uint;
typedef int int4[4];
enum ns { NS_X, NS_Y };
typedef enum ns ns_t;
enum flags { FL_A = 1, FL_B = 2 };
typedef struct s s;
struct s { int v; };
typedef struct t_tag *t_handle;
struct t_tag { long a; };
typedef int (*cmp_fn)(const void *, const void *);
struct mix {
  _Bool b; enum ns e; enum flags f; ns_t et; void *p; struct s inner; int arr[2];
  cmp_fn fn; int4 quad; uint u; char c; float fl; double d; s by_typedef;
  union { struct { short lo; short hi; } parts; int whole; } anon;
};
struct __attribute__((packed)) packed_s { char c; int i; };
struct aligned_s { char c; int i __attribute__((aligned(8))); };
struct with_bits { int a; unsigned b : 4; unsigned c : 4; long d; };
extern const char *g_cstr;
extern cmp_fn g_fn;
extern const struct s g_const_s;
extern int4 g_quad;
extern enum flags g_flags;
extern ns_t g_ns;
void take_void(void *p, const void *cp);
void take_cstr(cstr s, char *mutable, const unsigned char *bytes, const signed char *sbytes);
_Bool ret_bool(void);
char ret_char(void);
float ret_float(float f);
void take_enum(enum flags f, enum ns n, ns_t t);
void take_quad(int4 q, struct s *sp, s *sp2, t_handle h);
void unnamed(int, char *);
void mixed_names(int a, int);
const int ret_const(const int x);
struct s ret_s(void);
s ret_s2(s value);
#define M_NEG_FLOAT (-1.5)
#define M_ULL 0xFFFFFFFFFFFFFFFFULL
#define M_CAST_TYPEDEF ((uint)3)
#define M_ENUM_EXPR (NS_Y + 1)
#define M_CHAR_CAST ((char)65)
#define M_SHORT ((short)-2)
#define M_BOOL_CAST ((_Bool)1)
#define M_STRING_PAREN ("x")
#define M_DOUBLE_EXPR (1.0 / 4)
#define M_FLOAT_INT_MIX (2 * 1.5f)
#define M_DEFINED_LATER LATER_VALUE
#define LATER_VALUE 7
#define M_U8 u8"utf"
#define M_WIDE L"wide"
#define M_CHAR_ONLY 'z'
#define M_LONG_NEG (-5L)
