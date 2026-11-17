/* PLATFORM-SPECIFIC: a host must balance one lifetime around the whole OS-thread routine. */
#include "krusty_rt.h"

void kt_program_entry(void) {
    (void)kt_thread_lifetime_begin();
    (void)kt_thread_lifetime_begin();
}
