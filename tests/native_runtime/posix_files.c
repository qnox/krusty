/* Files through the POSIX layer: descriptors and their errno, `stat` in glibc's layout, a
   directory listing, `stdio`, and the special files a server reads at start. */
#include "driver_posix.h"

#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>

static void check_failures_set_errno(void) {
    errno = 0;
    DRIVER_CHECK(open("/nonexistent/krusty", O_RDONLY) == -1, "opening a missing file succeeded");
    DRIVER_CHECK(errno == ENOENT, "a missing file is not ENOENT");
    DRIVER_CHECK(close(-1) == -1 && errno == EBADF, "closing no descriptor is not EBADF");
    struct stat status;
    DRIVER_CHECK(stat("/nonexistent/krusty", &status) == -1 && errno == ENOENT,
                 "stat of a missing file is not ENOENT");
}

static void check_random_bytes(void) {
    int fd = open("/dev/urandom", O_RDONLY | O_CLOEXEC);
    DRIVER_CHECK(fd >= 0, "/dev/urandom did not open");
    unsigned char bytes[64];
    DRIVER_CHECK(read(fd, bytes, sizeof bytes) == sizeof bytes, "/dev/urandom gave a short read");
    int zeros = 0;
    for (unsigned i = 0; i < sizeof bytes; i++) {
        zeros += bytes[i] == 0;
    }
    DRIVER_CHECK(zeros < 16, "/dev/urandom gave mostly zeros");
    DRIVER_CHECK(fcntl(fd, F_GETFD) == FD_CLOEXEC, "O_CLOEXEC is not the descriptor's flag");
    DRIVER_CHECK(close(fd) == 0, "/dev/urandom did not close");
}

/* A server's own memory, read from /proc a line at a time. */
static void check_proc_status(void) {
    FILE *status = fopen("/proc/self/status", "r");
    DRIVER_CHECK(status != NULL, "/proc/self/status did not open");
    char line[4096];
    int found = 0;
    while (fgets(line, sizeof line, status) != NULL) {
        size_t length = strlen(line);
        DRIVER_CHECK(length > 0 && line[length - 1] == '\n', "fgets split a line");
        if (strncmp(line, "VmRSS:", 6) == 0 || strncmp(line, "Threads:", 8) == 0) {
            found++;
        }
    }
    DRIVER_CHECK(found == 2, "/proc/self/status lacks VmRSS or Threads");
    DRIVER_CHECK(feof(status) && !ferror(status), "the end of /proc/self/status is not EOF");
    DRIVER_CHECK(fclose(status) == 0, "/proc/self/status did not close");
}

static void check_stat_layout(void) {
    struct stat status;
    DRIVER_CHECK(stat("/", &status) == 0, "stat of / failed");
    DRIVER_CHECK(S_ISDIR(status.st_mode) && status.st_nlink >= 2, "/ is not a directory");
    DRIVER_CHECK(lstat("/proc/self/exe", &status) == 0 && S_ISLNK(status.st_mode),
                 "lstat followed /proc/self/exe");
    DRIVER_CHECK(stat("/proc/self/exe", &status) == 0 && S_ISREG(status.st_mode) &&
                     status.st_size > 0 && status.st_blksize > 0,
                 "stat did not follow /proc/self/exe to the executable");
}

static void check_a_directory_round_trip(void) {
    char directory[64] = "/tmp/krusty-posix-files-";
    size_t at = strlen(directory);
    for (int pid = getpid(); pid > 0; pid /= 10) {
        directory[at++] = (char)('0' + pid % 10);
    }
    directory[at] = '\0';
    DRIVER_CHECK(mkdir(directory, 0700) == 0, "mkdir failed");
    DRIVER_CHECK(mkdir(directory, 0700) == -1 && errno == EEXIST, "a second mkdir is not EEXIST");
    char path[96];
    strcpy(path, directory);
    strcat(path, "/written.txt");

    static const char text[] = "first line\nsecond line\n";
    FILE *out = fopen(path, "w");
    DRIVER_CHECK(out != NULL, "fopen for writing failed");
    DRIVER_CHECK(setvbuf(out, NULL, _IOFBF, 0) == 0, "setvbuf failed");
    DRIVER_CHECK(fwrite(text, 1, sizeof text - 1, out) == sizeof text - 1, "fwrite was short");
    DRIVER_CHECK(fputs("third\n", out) >= 0 && fflush(out) == 0, "fputs failed");
    DRIVER_CHECK(fclose(out) == 0, "fclose after writing failed");

    int fd = open(path, O_RDONLY);
    struct stat status;
    DRIVER_CHECK(fd >= 0 && fstat(fd, &status) == 0, "fstat failed");
    DRIVER_CHECK(S_ISREG(status.st_mode) && status.st_size == (off_t)(sizeof text - 1 + 6),
                 "fstat's size is not what was written");
    DRIVER_CHECK(lseek(fd, 6, SEEK_SET) == 6, "lseek failed");
    char word[4] = {0};
    DRIVER_CHECK(read(fd, word, 3) == 3 && memcmp(word, "lin", 3) == 0, "read after lseek");
    DRIVER_CHECK(close(fd) == 0, "close failed");

    FILE *in = fopen(path, "r");
    char everything[64] = {0};
    DRIVER_CHECK(in != NULL, "fopen for reading failed");
    DRIVER_CHECK(fread(everything, 1, sizeof everything, in) == sizeof text - 1 + 6,
                 "fread did not read the whole file");
    DRIVER_CHECK(memcmp(everything, "first line\nsecond line\nthird\n", 29) == 0,
                 "fread's bytes are not what was written");
    DRIVER_CHECK(fclose(in) == 0, "fclose after reading failed");

    DIR *listing = opendir(directory);
    DRIVER_CHECK(listing != NULL, "opendir failed");
    int dots = 0, files = 0;
    struct dirent *entry;
    while ((entry = readdir(listing)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) {
            dots++;
            DRIVER_CHECK(entry->d_type == DT_DIR, "a dot entry is not a directory");
        } else {
            DRIVER_CHECK(strcmp(entry->d_name, "written.txt") == 0 && entry->d_type == DT_REG,
                         "readdir named a file that is not there");
            files++;
        }
    }
    DRIVER_CHECK(dots == 2 && files == 1, "readdir did not list the directory");
    DRIVER_CHECK(closedir(listing) == 0, "closedir failed");
    DRIVER_CHECK(opendir(path) == NULL && errno == ENOTDIR, "opendir of a file is not ENOTDIR");

    DRIVER_CHECK(rmdir(directory) == -1 && errno == ENOTEMPTY, "rmdir of a full directory");
    DRIVER_CHECK(unlink(path) == 0, "unlink failed");
    DRIVER_CHECK(rmdir(directory) == 0, "rmdir failed");
}

static void check_a_pipe(void) {
    int ends[2];
    DRIVER_CHECK(pipe2(ends, O_CLOEXEC | O_NONBLOCK) == 0, "pipe2 failed");
    char byte;
    DRIVER_CHECK(read(ends[0], &byte, 1) == -1 && errno == EAGAIN, "an empty pipe is not EAGAIN");
    DRIVER_CHECK(write(ends[1], "k", 1) == 1 && read(ends[0], &byte, 1) == 1 && byte == 'k',
                 "a byte did not cross the pipe");
    DRIVER_CHECK(close(ends[1]) == 0 && read(ends[0], &byte, 1) == 0, "a closed pipe is not EOF");
    DRIVER_CHECK(close(ends[0]) == 0, "closing the pipe failed");
}

/* fcntl's third argument is absent, an int or a pointer by command, and an unknown command is
   EINVAL: each kind is read as what it is. */
static void check_fcntl_commands(void) {
    int ends[2];
    DRIVER_CHECK(pipe(ends) == 0, "pipe failed");
    DRIVER_CHECK(fcntl(ends[0], F_GETFD) == 0, "a new descriptor has FD_CLOEXEC");
    DRIVER_CHECK(fcntl(ends[0], F_SETFD, FD_CLOEXEC) == 0, "F_SETFD failed");
    DRIVER_CHECK(fcntl(ends[0], F_GETFD) == FD_CLOEXEC, "F_SETFD did not set FD_CLOEXEC");
    DRIVER_CHECK(fcntl(ends[0], F_SETFD, 0) == 0 && fcntl(ends[0], F_GETFD) == 0,
                 "F_SETFD did not clear FD_CLOEXEC");
    DRIVER_CHECK((fcntl(ends[0], F_GETFL) & O_NONBLOCK) == 0, "a new pipe is non-blocking");
    DRIVER_CHECK(fcntl(ends[0], F_SETFL, O_NONBLOCK) == 0, "F_SETFL failed");
    DRIVER_CHECK((fcntl(ends[0], F_GETFL) & O_NONBLOCK) == O_NONBLOCK,
                 "F_SETFL did not set O_NONBLOCK");
    int copy = fcntl(ends[0], F_DUPFD_CLOEXEC, 100);
    DRIVER_CHECK(copy >= 100 && fcntl(copy, F_GETFD) == FD_CLOEXEC,
                 "F_DUPFD_CLOEXEC did not take its lowest descriptor");
    DRIVER_CHECK(close(copy) == 0 && close(ends[0]) == 0 && close(ends[1]) == 0,
                 "closing the pipe failed");

    char path[64] = "/tmp/krusty-posix-lock-";
    size_t at = strlen(path);
    for (int pid = getpid(); pid > 0; pid /= 10) {
        path[at++] = (char)('0' + pid % 10);
    }
    path[at] = '\0';
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    DRIVER_CHECK(fd >= 0, "the lock file did not open");
    struct flock lock;
    memset(&lock, 0, sizeof lock);
    lock.l_type = F_WRLCK;
    lock.l_whence = SEEK_SET;
    lock.l_start = 0;
    lock.l_len = 10;
    DRIVER_CHECK(fcntl(fd, F_SETLK, &lock) == 0, "F_SETLK failed");
    /* A process's own lock never conflicts with it, so F_GETLK writes F_UNLCK back. */
    struct flock probe;
    memset(&probe, 0, sizeof probe);
    probe.l_type = F_WRLCK;
    probe.l_whence = SEEK_SET;
    probe.l_len = 10;
    DRIVER_CHECK(fcntl(fd, F_GETLK, &probe) == 0 && probe.l_type == F_UNLCK,
                 "F_GETLK did not answer through its pointer");
    DRIVER_CHECK(close(fd) == 0 && unlink(path) == 0, "removing the lock file failed");

    errno = 0;
    DRIVER_CHECK(fcntl(0, 12345) == -1 && errno == EINVAL, "an unknown command is not EINVAL");
}

void kt_program_entry(void) {
    check_failures_set_errno();
    check_random_bytes();
    check_proc_status();
    check_stat_layout();
    check_a_directory_round_trip();
    check_a_pipe();
    check_fcntl_commands();
    fputs("OK\n", stdout);
}
