/* PLATFORM-SPECIFIC: ending an already ended embedding lifetime is an error. */
#include "krusty_rt.h"

void kt_program_entry(void) {
    KThread *thread = kt_thread_lifetime_begin();
    kt_thread_lifetime_end(thread);
    kt_thread_lifetime_end(thread);
}
