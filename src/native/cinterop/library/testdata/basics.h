typedef unsigned long size_t;
#define SMALL_LIMIT 42
#define SMALL_NAME "small"
#define SMALL_PI 3.5
#define SMALL_MASK (1u << 4)
#define SMALL_EXPR (small_counter + 1)
typedef unsigned int small_flags;
typedef struct point { int x; int y; } point_t;
struct bits { unsigned a : 3; unsigned b : 5; long c; };
enum color { RED, GREEN = 5, BLUE };
enum { ANON_A = 1, ANON_B = 2 };
typedef enum { MODE_X, MODE_Y } mode_t2;
enum strict_flags { F_ONE = 1, F_TWO = 2, F_ALIAS = 1 };
typedef int (*small_callback)(const char *name, void *user);
struct opaque;
extern int small_counter;
extern const char *small_names[4];
int small_printf(const char *format, ...);
double small_area(point_t p, struct bits *b);
void small_register(small_callback cb, struct opaque *handle);
enum color small_pick(mode_t2 mode, small_flags flags, size_t size, unsigned char *bytes);
