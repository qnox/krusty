/* A program whose entry RETURNS ends with status 0, as a Kotlin `main` that returns does. `_start`
   used to follow the call with a trapping instruction, so a normal end was reported as a crash. */
#include "krusty_sys.h"

void kt_program_entry(void) { kt_sys_write(1, "OK\n", 3); }
