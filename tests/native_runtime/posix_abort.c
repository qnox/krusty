/* `abort` with SIGABRT blocked: it must still end the process on SIGABRT, not leave the signal
   pending and exit normally. The test checks how the process ended. */
#include "driver_posix.h"

#include <signal.h>
#include <stdlib.h>

void kt_program_entry(void) {
    sigset_t abort_only;
    sigemptyset(&abort_only);
    sigaddset(&abort_only, SIGABRT);
    DRIVER_CHECK(sigprocmask(SIG_BLOCK, &abort_only, NULL) == 0, "blocking SIGABRT failed");
    abort();
}
