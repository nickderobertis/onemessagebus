/* Preloaded into one spool journey's receiver process: shifts that process's
 * wall clock (CLOCK_REALTIME) by ONEMESSAGEBUS_WALL_CLOCK_SHIFT_SECONDS, as a
 * system clock stepped forward or back would, and leaves every other clock,
 * and every other process, as it is. Built by `offer.rs` with the system C
 * compiler. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <time.h>

int clock_gettime(clockid_t clock, struct timespec *reading) {
    static int (*real)(clockid_t, struct timespec *);
    if (!real) {
        real = (int (*)(clockid_t, struct timespec *))dlsym(RTLD_NEXT, "clock_gettime");
    }
    int read = real(clock, reading);
    if (read == 0 && (clock == CLOCK_REALTIME || clock == CLOCK_REALTIME_COARSE)) {
        const char *shift = getenv("ONEMESSAGEBUS_WALL_CLOCK_SHIFT_SECONDS");
        if (shift) {
            reading->tv_sec += atoll(shift);
        }
    }
    return read;
}
