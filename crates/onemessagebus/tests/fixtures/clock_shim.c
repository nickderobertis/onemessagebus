/* Preloaded into a spool journey's child processes, to do to them what a
 * journey cannot do to the host:
 * - ONEMESSAGEBUS_WALL_CLOCK_SHIFT_FILE names a file holding a whole number of
 *   seconds the process's wall clock (CLOCK_REALTIME) is shifted by, read at
 *   every reading, so every process preloaded with one file reads one system
 *   clock stepped forward or back, and every other clock is as it is. Absent,
 *   the shift is none; anything but a whole number aborts the process.
 * - ONEMESSAGEBUS_HIDE=boot-id makes /proc/sys/kernel/random/boot_id
 *   unreadable, and ONEMESSAGEBUS_HIDE=boot-id-malformed reads it as no UUID.
 * - ONEMESSAGEBUS_HIDE=time-namespace makes /proc/self/ns/time absent, as on a
 *   kernel without time namespaces; ONEMESSAGEBUS_HIDE=time-namespace-denied
 *   makes it unreadable, and ONEMESSAGEBUS_HIDE=time-namespace-malformed
 *   links it to no namespace inode.
 * Built by `offer.rs` with the system C compiler. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

static int hides(const char *what, const char *path, const char *hidden) {
    const char *hide = getenv("ONEMESSAGEBUS_HIDE");
    return hide && strcmp(hide, what) == 0 && strcmp(path, hidden) == 0;
}

static const char BOOT_ID[] = "/proc/sys/kernel/random/boot_id";

static int boot_id_hidden(const char *path) {
    return hides("boot-id", path, BOOT_ID);
}

/* A boot id that is no UUID, read from a pipe in place of the kernel's. */
static int malformed_boot_id(const char *path) {
    if (!hides("boot-id-malformed", path, BOOT_ID)) {
        return -1;
    }
    int ends[2];
    if (pipe(ends) != 0) {
        abort();
    }
    static const char text[] = "not-a-boot-id\n";
    if (write(ends[1], text, sizeof text - 1) != (ssize_t)(sizeof text - 1)) {
        abort();
    }
    close(ends[1]);
    return ends[0];
}

static mode_t mode_of(int flags, va_list rest) {
    return (flags & (O_CREAT | O_TMPFILE)) ? (mode_t)va_arg(rest, unsigned int) : 0;
}

int open64(const char *path, int flags, ...) {
    static int (*real)(const char *, int, ...);
    if (!real) {
        real = (int (*)(const char *, int, ...))dlsym(RTLD_NEXT, "open64");
    }
    va_list rest;
    va_start(rest, flags);
    mode_t mode = mode_of(flags, rest);
    va_end(rest);
    if (boot_id_hidden(path)) {
        errno = EACCES;
        return -1;
    }
    int malformed = malformed_boot_id(path);
    if (malformed >= 0) {
        return malformed;
    }
    return real(path, flags, mode);
}

int open(const char *path, int flags, ...) {
    static int (*real)(const char *, int, ...);
    if (!real) {
        real = (int (*)(const char *, int, ...))dlsym(RTLD_NEXT, "open");
    }
    va_list rest;
    va_start(rest, flags);
    mode_t mode = mode_of(flags, rest);
    va_end(rest);
    if (boot_id_hidden(path)) {
        errno = EACCES;
        return -1;
    }
    int malformed = malformed_boot_id(path);
    if (malformed >= 0) {
        return malformed;
    }
    return real(path, flags, mode);
}

ssize_t readlink(const char *path, char *buffer, size_t size) {
    static ssize_t (*real)(const char *, char *, size_t);
    if (!real) {
        real = (ssize_t (*)(const char *, char *, size_t))dlsym(RTLD_NEXT, "readlink");
    }
    static const char TIME_NAMESPACE[] = "/proc/self/ns/time";
    if (hides("time-namespace", path, TIME_NAMESPACE)) {
        errno = ENOENT;
        return -1;
    }
    if (hides("time-namespace-denied", path, TIME_NAMESPACE)) {
        errno = EACCES;
        return -1;
    }
    if (hides("time-namespace-malformed", path, TIME_NAMESPACE)) {
        static const char link[] = "time:[not-an-inode]";
        size_t length = sizeof link - 1 < size ? sizeof link - 1 : size;
        memcpy(buffer, link, length);
        return (ssize_t)length;
    }
    return real(path, buffer, size);
}

/* The shift the file names, in seconds: none without one. */
static long long shift(void) {
    const char *named = getenv("ONEMESSAGEBUS_WALL_CLOCK_SHIFT_FILE");
    if (!named) {
        return 0;
    }
    FILE *file = fopen(named, "r");
    if (!file) {
        return 0;
    }
    /* One byte more than any whole number of seconds needs, so a longer file
       is seen to be one rather than read in part. */
    char text[33] = {0};
    size_t read = fread(text, 1, sizeof text - 1, file);
    int longer = read == sizeof text - 1;
    fclose(file);
    text[read] = '\0';
    char *end = NULL;
    errno = 0;
    long long seconds = strtoll(text, &end, 10);
    /* Converted first, so a file of whitespace alone is no number. */
    int converted = errno == 0 && end != text;
    while (converted && (*end == '\n' || *end == ' ')) {
        end++;
    }
    /* Ending at the bytes read, so an embedded NUL hides nothing after it. */
    if (longer || !converted || end != text + read) {
        fprintf(stderr, "clock_shim: %s holds %s, not a whole number of seconds\n", named, text);
        abort();
    }
    return seconds;
}

int clock_gettime(clockid_t clock, struct timespec *reading) {
    static int (*real)(clockid_t, struct timespec *);
    if (!real) {
        real = (int (*)(clockid_t, struct timespec *))dlsym(RTLD_NEXT, "clock_gettime");
    }
    int got = real(clock, reading);
    if (got == 0 && (clock == CLOCK_REALTIME || clock == CLOCK_REALTIME_COARSE)) {
        long long seconds = shift();
        if ((seconds > 0 && reading->tv_sec > LLONG_MAX - seconds)
            || (seconds < 0 && reading->tv_sec < LLONG_MIN - seconds)) {
            fprintf(stderr, "clock_shim: a shift of %llds overflows the clock\n", seconds);
            abort();
        }
        reading->tv_sec += seconds;
    }
    return got;
}
