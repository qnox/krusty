/* An exception a started thread leaves in flight ends the process, reported on that thread's
   name, as Kotlin reports an exception nothing caught. */
#include "krusty_rt.h"
#include "krusty_sys.h"

static void quiet(KRef argument) { (void)argument; }

static void raising(KRef argument) {
    (void)argument;
    kt_throw(kt_throwable_new(&kt_type_illegal_state_exception, kt_string_utf8("boom", 4)));
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    kt_thread_join(kt_thread_start(quiet, NULL));
    kt_thread_join(kt_thread_start(raising, NULL));
    kt_sys_write(1, "unreachable\n", 12);
}
