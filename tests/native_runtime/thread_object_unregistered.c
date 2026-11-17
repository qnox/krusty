/* PLATFORM-SPECIFIC: legacy temporary callbacks do not claim OS-thread persistence. */
#include "krusty_rt.h"

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    static const char declaration;
    KThreadEntry entry;
    kt_callback_enter(&entry, &bottom);
    (void)kt_thread_object_slot(&declaration);
    kt_callback_leave(&entry);
}
