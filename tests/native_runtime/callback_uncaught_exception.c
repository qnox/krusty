/* An exception a callback leaves in flight ends the process, with the lock still held: the foreign
   code the callback returns to has no way to handle a Kotlin exception, and a heap reference handed
   to it after the lock is released would be a root to nobody while another thread collects. The
   report names the thread, numbered as the JVM numbers a thread it did not start. While the worker
   raises, the main thread collects continuously with the lock released between rounds, so a
   collection runs whenever the worker's leave would let one. */
#include "driver_threads.h"

static void raising(void *argument) {
    (void)argument;
    uintptr_t bottom = 0;
    KThreadEntry entry;
    kt_callback_enter(&entry, &bottom);
    kt_throw(kt_throwable_new(&kt_type_illegal_state_exception, kt_string_utf8("leaked", 6)));
    kt_callback_leave(&entry);
    kt_sys_write(1, "unreachable\n", 12);
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    DriverThread thread;
    driver_thread_start(&thread, raising, NULL);
    for (;;) {
        (void)kt_throwable_new(&kt_type_illegal_state_exception, NULL);
        kt_gc_collect();
        KThread *self = kt_native_enter();
        kt_syscall(KT_SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
        kt_native_leave(self);
    }
}
