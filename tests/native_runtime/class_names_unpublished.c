/* A class whose descriptor publishes no reflection names has no answer to `simpleName` that is not
   a guess, so asking is a loud failure that names the descriptor, rather than a name made up by
   splitting its rendered one. */
#include "standins.h"

static const KType unpublished_type = {.name = "pkg.Unpublished",
                                       .name_length = sizeof("pkg.Unpublished") - 1,
                                       .instance_size = sizeof(KObjectHeader),
                                       .super = &kt_type_any};

void kt_program_entry(void) {
    DRIVER_BEGIN();
    (void)kt_class_simple_name(kt_class_literal(&unpublished_type));
    kt_sys_write(1, "OK\n", 3);
}
