use super::super::compiler_headers::preprocessor_for;
use super::super::model::{Declarations, Storage, Type};
use super::*;
use crate::native::target::{Arch, Os};

const X86_64: NativeTarget = NativeTarget::new(Arch::X86_64, Os::Linux);
const AARCH64: NativeTarget = NativeTarget::new(Arch::Aarch64, Os::Linux);

fn try_parse(target: NativeTarget, source: &str) -> Result<Header, ParseError> {
    let mut preprocessor = preprocessor_for(target, Vec::new(), Vec::new()).expect("predefines");
    let tokens = preprocessor.run("t.h", source).expect("preprocesses");
    parse(&tokens, target)
}

fn parse_ok(target: NativeTarget, source: &str) -> Declarations {
    match try_parse(target, source) {
        Ok(header) => header.declarations,
        Err(error) => panic!("{error}"),
    }
}

/// Each function as `name: type`, with what the declaration adds.
fn functions(declarations: &Declarations) -> Vec<String> {
    declarations
        .functions
        .iter()
        .map(|function| {
            let names: Vec<&str> = function
                .param_names
                .iter()
                .map(|name| name.as_deref().unwrap_or("_"))
                .collect();
            let mut line = format!(
                "{}: {} ({})",
                function.name,
                declarations.spell(function.ty),
                names.join(", ")
            );
            if function.storage == Storage::Static {
                line.push_str(" static");
            }
            if function.inline {
                line.push_str(" inline");
            }
            if function.has_body {
                line.push_str(" body");
            }
            if function.noreturn {
                line.push_str(" noreturn");
            }
            if let Some(asm) = &function.asm_name {
                line.push_str(&format!(" asm={asm}"));
            }
            line
        })
        .collect()
}

fn typedefs(declarations: &Declarations) -> Vec<String> {
    declarations
        .typedefs
        .iter()
        .filter(|typedef| typedef.origin.is_some())
        .map(|typedef| {
            let aligned = typedef
                .aligned
                .map_or(String::new(), |aligned| format!(" aligned({aligned})"));
            format!(
                "{} = {}{aligned}",
                typedef.name,
                declarations.spell(typedef.ty)
            )
        })
        .collect()
}

#[test]
fn functions_take_their_prototype_attributes_and_asm_labels() {
    let declarations = parse_ok(
        X86_64,
        "extern int printf (const char *__restrict __format, ...)\n\
             __attribute__ ((__format__ (__printf__, 1, 2))) __attribute__ ((__nonnull__ (1)));\n\
         extern void (*signal (int __sig, void (*__handler) (int))) (int);\n\
         int old ();\n\
         extern int stat64_like (int) __asm__ (\"\" \"stat64\") __attribute__ ((__nothrow__));\n\
         static __inline int twice (int x) { return x * 2; }\n\
         _Noreturn void quit (int);\n\
         extern void abort (void) __attribute__ ((__noreturn__));\n\
         typedef int handler_t (int);\n\
         void apply (handler_t h, int a[static 4], int n, char s[n], const int c);\n\
         int printf (const char *, ...);\n",
    );
    assert_eq!(
        functions(&declarations),
        [
            "printf: int (const char *, ...) (__format)",
            "signal: void (*(int, void (*)(int)))(int) (__sig, __handler)",
            "old: int () ()",
            "stat64_like: int (int) (_) asm=stat64",
            "twice: int (int) (x) static inline body",
            "quit: void (int) (_) noreturn",
            "abort: void (void) () noreturn",
            "apply: void (handler_t *, int *, int, char *, int) (h, a, n, s, c)",
        ]
    );
}

#[test]
fn typedefs_read_gnu_mode_extension_and_typeof() {
    let declarations = parse_ok(
        X86_64,
        "__extension__ typedef long long int quad;\n\
         typedef int int8 __attribute__ ((__mode__ (__QI__)));\n\
         typedef unsigned int u64 __attribute__ ((__mode__ (__DI__)));\n\
         typedef int word __attribute__ ((__mode__ (__word__)));\n\
         typedef long aligned4 __attribute__ ((aligned (4)));\n\
         extern int counter;\n\
         typedef __typeof__ (counter) counter_t;\n\
         typedef __typeof__ (sizeof (int)) size_type;\n\
         typedef char (*table)[3 * 4];\n\
         typedef _Atomic (int) atomic_int_t;\n\
         typedef const volatile int cv;\n\
         typedef __builtin_va_list va;\n\
         typedef float v4 __attribute__ ((vector_size (16)));\n\
         typedef _Complex double cd;\n\
         typedef quad quad;\n",
    );
    assert_eq!(
        typedefs(&declarations),
        [
            "quad = long long",
            "int8 = signed char",
            "u64 = unsigned long",
            "word = long",
            "aligned4 = long aligned(4)",
            "counter_t = int",
            "size_type = unsigned long",
            "table = char (*)[12]",
            "atomic_int_t = _Atomic(int)",
            "cv = const volatile int",
            "va = __builtin_va_list",
            "v4 = float __attribute__((vector_size(16)))",
            "cd = _Complex double",
        ]
    );
}

#[test]
fn enumerators_are_constant_expressions_in_cs_types() {
    let declarations = parse_ok(
        AARCH64,
        "enum e { A, B = 5, C, D = (1 << 4) | B, E = -2, F = sizeof (long) * 2,\n\
                  G = _Alignof (double), H = 'A', I = '\\xff', J = (unsigned char) -1,\n\
                  K = F > 8 ? 1 : 2, L = 0xffffffffu + 1, M = -1 / 2, N = ~0u >> 28,\n\
                  O = __builtin_offsetof (struct s { char c; int x[4]; }, x[2]) };\n",
    );
    let enumeration = &declarations.enums[0];
    let values: Vec<String> = enumeration
        .enumerators
        .as_ref()
        .expect("defined")
        .iter()
        .map(|enumerator| format!("{}={}", enumerator.name, enumerator.value))
        .collect();
    // aarch64's plain `char` is unsigned, so '\xff' is 255 there.
    assert_eq!(
        values,
        [
            "A=0", "B=5", "C=6", "D=21", "E=-2", "F=16", "G=8", "H=65", "I=255", "J=255", "K=1",
            "L=0", "M=0", "N=15", "O=12"
        ]
    );
}

#[test]
fn a_plain_char_constant_takes_the_targets_signedness() {
    let declarations = parse_ok(X86_64, "enum { I = '\\xff' };");
    let enumerators = declarations.enums[0].enumerators.as_ref().expect("defined");
    assert_eq!(enumerators[0].value, -1);
}

#[test]
fn records_keep_bitfields_anonymous_members_and_attributes() {
    let declarations = parse_ok(
        X86_64,
        "struct node { struct node *next; unsigned flags : 3, : 0; int : 2;\n\
             union { int i; float f; }; struct { char a, b; } named;\n\
             long value __attribute__ ((aligned (16))); char tail[]; }\n\
             __attribute__ ((__packed__));\n\
         typedef struct { int x; } point;\n\
         struct later;\n\
         struct later *forward (void);\n",
    );
    let summary: Vec<String> = declarations
        .records
        .iter()
        .map(|record| {
            let fields: Vec<String> = record
                .fields
                .as_ref()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|field| {
                            let mut text = format!(
                                "{} {}",
                                declarations.spell(field.ty),
                                field.name.as_deref().unwrap_or("_")
                            );
                            if let Some(width) = field.bit_width {
                                text.push_str(&format!(":{width}"));
                            }
                            if let Some(aligned) = field.aligned {
                                text.push_str(&format!(" aligned({aligned})"));
                            }
                            text
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "{} packed={} [{}]",
                record.tag.as_deref().unwrap_or("_"),
                record.packed,
                fields.join("; ")
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            "node packed=true [struct node * next; unsigned int flags:3; unsigned int _:0; \
             int _:2; union <anonymous> _; struct <anonymous> named; long value aligned(16); \
             char [] tail]",
            "_ packed=false [int i; float f]",
            "_ packed=false [char a; char b]",
            "_ packed=false [int x]",
            "later packed=false []",
        ]
    );
    assert_eq!(
        declarations.member_path(super::super::model::RecordId(0), "f"),
        Some(vec![4, 1])
    );
    assert_eq!(
        functions(&declarations),
        ["forward: struct later *(void) ()"]
    );
}

#[test]
fn variables_merge_redeclarations_and_keep_thread_storage() {
    let declarations = parse_ok(
        X86_64,
        "extern char *optarg;\n\
         extern __thread int depth;\n\
         extern int table[];\n\
         int table[4];\n\
         static const int limits[2] = { 1, 2 }, single = 3;\n\
         extern _Alignas (16) char buffer[3];\n\
         extern long tz __asm__ (\"__timezone\");\n",
    );
    let summary: Vec<String> = declarations
        .variables
        .iter()
        .map(|variable| {
            format!(
                "{}: {} {:?} tls={} aligned={:?} asm={:?}",
                variable.name,
                declarations.spell(variable.ty),
                variable.storage,
                variable.thread_local,
                variable.aligned,
                variable.asm_name.as_deref()
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            "optarg: char * Extern tls=false aligned=None asm=None",
            "depth: int Extern tls=true aligned=None asm=None",
            "table: int [4] Extern tls=false aligned=None asm=None",
            "limits: const int [2] Static tls=false aligned=None asm=None",
            "single: const int Static tls=false aligned=None asm=None",
            "buffer: char [3] Extern tls=false aligned=Some(16) asm=None",
            "tz: long Extern tls=false aligned=None asm=Some(\"__timezone\")",
        ]
    );
}

#[test]
fn a_typedef_name_is_a_declarator_after_a_type_specifier() {
    let declarations = parse_ok(
        X86_64,
        "typedef int size;\nstruct s { size size; };\nint f (int size, size other);\n",
    );
    let record = &declarations.records[0];
    let field = &record.fields.as_ref().expect("defined")[0];
    assert_eq!(field.name.as_deref(), Some("size"));
    assert!(matches!(declarations.ty(field.ty), Type::Typedef(_)));
    assert_eq!(
        functions(&declarations),
        ["f: int (int, size) (size, other)"]
    );
}

#[test]
fn errors_name_the_line_and_what_was_wrong() {
    let error = |source: &str| match try_parse(X86_64, source) {
        Ok(_) => panic!("parsed: {source}"),
        Err(error) => error.to_string(),
    };
    assert_eq!(
        error("int a;\nmystery_t b;\n"),
        "t.h:2: type specifier missing, found 'mystery_t'"
    );
    assert_eq!(
        error("_Static_assert (sizeof (int) == 8, \"wide\");"),
        "t.h:1: static assertion failed"
    );
    assert_eq!(
        error("struct s { int a; };\nstruct s { int b; };"),
        "t.h:2: redefinition of 's'"
    );
    assert_eq!(
        error("struct s;\nint n[sizeof (struct s)];"),
        "t.h:2: `struct s` is an incomplete type"
    );
    assert_eq!(
        error("typedef int t;\ntypedef long t;"),
        "t.h:2: typedef redefinition with different types ('long' vs 'int')"
    );
    assert_eq!(error("enum { A = 1 / 0 };"), "t.h:1: division by zero");
    assert_eq!(error("int f (int) {"), "t.h:1: unbalanced brackets");
}
