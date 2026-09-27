/* `KClass.simpleName` and `qualifiedName` answer what the class's descriptor publishes, and `null`
   where Kotlin answers `null`. They used to split the descriptor's rendered name on `.` and `$`,
   which answered `b` for a class declared as `a$b` and a non-null qualified name for every class,
   local and anonymous ones included.

   The expected names are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       package pkg
       class Top { class Nested }
       class `a$b`
       fun main() {
           println(Top::class.simpleName + " " + Top::class.qualifiedName)
           println(Top.Nested::class.simpleName + " " + Top.Nested::class.qualifiedName)
           println(`a$b`::class.simpleName + " " + `a$b`::class.qualifiedName)
           class Local
           println(Local::class.simpleName + " " + Local::class.qualifiedName)
           val anon = object {}
           println(anon::class.simpleName + " " + anon::class.qualifiedName)
           println(String::class.simpleName + " " + String::class.qualifiedName)
           println(Int::class.simpleName + " " + Int::class.qualifiedName)
       }

   which prints `Top pkg.Top`, `Nested pkg.Top.Nested`, `a$b pkg.a$b`, `Local null`,
   `null null`, `String kotlin.String` and `Int kotlin.Int`. */
#include "later_tiers.h"

/* Descriptors as a compiler emits them for those classes: each `name` is the rendered name, which
   for the nested, local and anonymous classes is deliberately not the identity, so that an answer
   read off it would be wrong. */
static const KType top_type = {.name = "pkg.Top",
                               .name_length = sizeof("pkg.Top") - 1,
                               .instance_size = sizeof(KObjectHeader),
                               .super = &kt_type_any,
                               .qualified_name = "pkg.Top",
                               .qualified_name_length = sizeof("pkg.Top") - 1,
                               .simple_name = "Top",
                               .simple_name_length = sizeof("Top") - 1,
                               .class_names = KT_CLASS_NAMES_MEMBER};

static const KType nested_type = {.name = "pkg.Top$Nested",
                                  .name_length = sizeof("pkg.Top$Nested") - 1,
                                  .instance_size = sizeof(KObjectHeader),
                                  .super = &kt_type_any,
                                  .qualified_name = "pkg.Top.Nested",
                                  .qualified_name_length = sizeof("pkg.Top.Nested") - 1,
                                  .simple_name = "Nested",
                                  .simple_name_length = sizeof("Nested") - 1,
                                  .class_names = KT_CLASS_NAMES_MEMBER};

static const KType dollar_type = {.name = "pkg.a$b",
                                  .name_length = sizeof("pkg.a$b") - 1,
                                  .instance_size = sizeof(KObjectHeader),
                                  .super = &kt_type_any,
                                  .qualified_name = "pkg.a$b",
                                  .qualified_name_length = sizeof("pkg.a$b") - 1,
                                  .simple_name = "a$b",
                                  .simple_name_length = sizeof("a$b") - 1,
                                  .class_names = KT_CLASS_NAMES_MEMBER};

static const KType local_type = {.name = "pkg.RefKt$main$Local",
                                 .name_length = sizeof("pkg.RefKt$main$Local") - 1,
                                 .instance_size = sizeof(KObjectHeader),
                                 .super = &kt_type_any,
                                 .simple_name = "Local",
                                 .simple_name_length = sizeof("Local") - 1,
                                 .class_names = KT_CLASS_NAMES_LOCAL};

static const KType anonymous_type = {.name = "pkg.RefKt$main$anon$1",
                                     .name_length = sizeof("pkg.RefKt$main$anon$1") - 1,
                                     .instance_size = sizeof(KObjectHeader),
                                     .super = &kt_type_any,
                                     .class_names = KT_CLASS_NAMES_ANONYMOUS};

/* Whether `text` is the string of exactly these bytes; NULL is Kotlin's `null` and never is. */
static kt_boolean names(KRef text, const char *expected, kt_int length) {
    return text != NULL && driver_text_is(text, expected, length);
}

#define NAMES(text, literal) names(text, literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef top = kt_class_literal(&top_type);
    DRIVER_CHECK(NAMES(kt_class_simple_name(top), "Top"), "Top::class.simpleName");
    DRIVER_CHECK(NAMES(kt_class_qualified_name(top), "pkg.Top"), "Top::class.qualifiedName");

    KRef nested = kt_class_literal(&nested_type);
    DRIVER_CHECK(NAMES(kt_class_simple_name(nested), "Nested"), "Top.Nested::class.simpleName");
    DRIVER_CHECK(NAMES(kt_class_qualified_name(nested), "pkg.Top.Nested"),
                 "Top.Nested::class.qualifiedName");

    KRef dollar = kt_class_literal(&dollar_type);
    DRIVER_CHECK(NAMES(kt_class_simple_name(dollar), "a$b"), "`a$b`::class.simpleName");
    DRIVER_CHECK(NAMES(kt_class_qualified_name(dollar), "pkg.a$b"), "`a$b`::class.qualifiedName");

    KRef local = kt_class_literal(&local_type);
    DRIVER_CHECK(NAMES(kt_class_simple_name(local), "Local"), "Local::class.simpleName");
    DRIVER_CHECK(kt_class_qualified_name(local) == NULL, "Local::class.qualifiedName is not null");

    KRef anonymous = kt_class_literal(&anonymous_type);
    DRIVER_CHECK(kt_class_simple_name(anonymous) == NULL, "anon::class.simpleName is not null");
    DRIVER_CHECK(kt_class_qualified_name(anonymous) == NULL,
                 "anon::class.qualifiedName is not null");

    /* The runtime's own classes publish theirs, and a value's class is its type's literal. */
    KRef string = kt_class_of(kt_string_utf8("x", 1));
    DRIVER_CHECK(NAMES(kt_class_simple_name(string), "String"), "String::class.simpleName");
    DRIVER_CHECK(NAMES(kt_class_qualified_name(string), "kotlin.String"),
                 "String::class.qualifiedName");
    DRIVER_CHECK(kt_equals(string, kt_class_literal(&kt_type_string)),
                 "\"x\"::class != String::class");
    DRIVER_CHECK(kt_hash_code(string) == kt_hash_code(kt_class_literal(&kt_type_string)),
                 "\"x\"::class and String::class hash apart");
    DRIVER_CHECK(!kt_equals(string, kt_class_literal(&kt_type_int)), "String::class == Int::class");
    KRef integer = kt_class_literal(&kt_type_int);
    DRIVER_CHECK(NAMES(kt_class_simple_name(integer), "Int"), "Int::class.simpleName");
    DRIVER_CHECK(NAMES(kt_class_qualified_name(integer), "kotlin.Int"), "Int::class.qualifiedName");
    KRef builder = kt_class_literal(&kt_type_string_builder);
    DRIVER_CHECK(NAMES(kt_class_simple_name(builder), "StringBuilder"),
                 "StringBuilder::class.simpleName");
    DRIVER_CHECK(NAMES(kt_class_qualified_name(builder), "kotlin.text.StringBuilder"),
                 "StringBuilder::class.qualifiedName");
    KRef pair = kt_class_literal(&kt_type_pair);
    DRIVER_CHECK(NAMES(kt_class_simple_name(pair), "Pair"), "Pair::class.simpleName");
    DRIVER_CHECK(NAMES(kt_class_qualified_name(pair), "kotlin.Pair"), "Pair::class.qualifiedName");

    DRIVER_CHECK(kt_pending_exception() == NULL, "asking a class for its names raised");
    kt_sys_write(1, "OK\n", 3);
}
