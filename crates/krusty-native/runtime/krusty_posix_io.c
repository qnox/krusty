/* krusty native runtime, POSIX layer: files. Descriptors, `stat`, directories, and a minimal
   `stdio`. Hand-written freestanding C; see krusty_posix.h. */
#include "krusty_posix.h"

/* ---- descriptors ------------------------------------------------------------------------------- */

int openat(int directory, const char *path, int flags, ...) {
    long mode = 0;
    if ((flags & KT_O_CREAT) != 0) {
        va_list rest;
        va_start(rest, flags);
        mode = va_arg(rest, int);
        va_end(rest);
    }
    return (int)kt_posix_result(kt_syscall(KT_SYS_OPENAT, directory, (long)path, flags, mode, 0, 0));
}

int open(const char *path, int flags, ...) {
    long mode = 0;
    if ((flags & KT_O_CREAT) != 0) {
        va_list rest;
        va_start(rest, flags);
        mode = va_arg(rest, int);
        va_end(rest);
    }
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_OPENAT, KT_AT_FDCWD, (long)path, flags, mode, 0, 0));
}

long read(int fd, void *bytes, size_t length) {
    return kt_posix_result(kt_syscall(KT_SYS_READ, fd, (long)bytes, (long)length, 0, 0, 0));
}

long write(int fd, const void *bytes, size_t length) {
    return kt_posix_result(kt_syscall(KT_SYS_WRITE, fd, (long)bytes, (long)length, 0, 0, 0));
}

int close(int fd) { return (int)kt_posix_result(kt_syscall(KT_SYS_CLOSE, fd, 0, 0, 0, 0, 0)); }

long lseek(int fd, long offset, int whence) {
    return kt_posix_result(kt_syscall(KT_SYS_LSEEK, fd, offset, whence, 0, 0, 0));
}

/* A command's argument is absent, an int, or a pointer, and is read as exactly that: a caller of
   `F_GETFD` passes nothing, and a variadic argument that was never passed cannot be read. A
   command the layer does not know is EINVAL, as the kernel answers one it does not know, rather
   than an argument read at a guessed type. `struct flock` is the kernel's on every supported
   target, so a lock command's pointer passes through. */
int fcntl(int fd, int command, ...) {
    long argument = 0;
    va_list rest;
    va_start(rest, command);
    switch (command) {
    case KT_F_GETFD:
    case KT_F_GETFL:
    case KT_F_GETOWN:
        break;
    case KT_F_DUPFD:
    case KT_F_DUPFD_CLOEXEC:
    case KT_F_SETFD:
    case KT_F_SETFL:
    case KT_F_SETOWN:
        argument = va_arg(rest, int);
        break;
    case KT_F_GETLK:
    case KT_F_SETLK:
    case KT_F_SETLKW:
        argument = (long)va_arg(rest, void *);
        break;
    default:
        va_end(rest);
        *__errno_location() = KT_EINVAL;
        return -1;
    }
    va_end(rest);
    return (int)kt_posix_result(kt_syscall(KT_SYS_FCNTL, fd, command, argument, 0, 0, 0));
}

int pipe2(int fds[2], int flags) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_PIPE2, (long)fds, flags, 0, 0, 0, 0));
}

int pipe(int fds[2]) { return pipe2(fds, 0); }

int unlink(const char *path) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_UNLINKAT, KT_AT_FDCWD, (long)path, 0, 0, 0, 0));
}

int rmdir(const char *path) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_UNLINKAT, KT_AT_FDCWD, (long)path, KT_AT_REMOVEDIR, 0, 0, 0));
}

int mkdir(const char *path, unsigned mode) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_MKDIRAT, KT_AT_FDCWD, (long)path, mode, 0, 0, 0));
}

/* ---- stat ---------------------------------------------------------------------------------------

   On each supported target, glibc's `struct stat` is the kernel's (`newfstatat` fills the 144-byte
   x86_64 layout there and the 128-byte generic one on aarch64 and riscv64), so the caller's buffer
   goes to the kernel as it is. */

int fstatat(int directory, const char *path, void *status, int flags) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_NEWFSTATAT, directory, (long)path, (long)status, flags, 0, 0));
}

int stat(const char *path, void *status) { return fstatat(KT_AT_FDCWD, path, status, 0); }

int lstat(const char *path, void *status) {
    return fstatat(KT_AT_FDCWD, path, status, KT_AT_SYMLINK_NOFOLLOW);
}

int fstat(int fd, void *status) { return fstatat(fd, "", status, KT_AT_EMPTY_PATH); }

/* ---- directories --------------------------------------------------------------------------------

   On a 64-bit target glibc's `struct dirent` is the kernel's `linux_dirent64` (inode, offset,
   record length, type, then the NUL-terminated name), so `readdir` answers a pointer straight into
   the buffer `getdents64` filled, as glibc does. */

#define KT_DIRECTORY_BUFFER 32768

typedef struct {
    int fd;
    size_t at;
    size_t end;
    char entries[KT_DIRECTORY_BUFFER] __attribute__((aligned(8)));
} KDirectory;

KDirectory *fdopendir(int fd) {
    KDirectory *directory = (KDirectory *)kt_map(sizeof(KDirectory));
    directory->fd = fd;
    return directory;
}

KDirectory *opendir(const char *path) {
    int fd = open(path, KT_O_RDONLY | KT_O_DIRECTORY | KT_O_CLOEXEC);
    return fd < 0 ? NULL : fdopendir(fd);
}

void *readdir(KDirectory *directory) {
    if (directory->at >= directory->end) {
        long filled = kt_syscall(KT_SYS_GETDENTS64, directory->fd, (long)directory->entries,
                                 KT_DIRECTORY_BUFFER, 0, 0, 0);
        if (filled <= 0) {
            /* The end, with errno untouched, or a failure, with it set. */
            kt_posix_result(filled);
            return NULL;
        }
        directory->at = 0;
        directory->end = (size_t)filled;
    }
    char *entry = directory->entries + directory->at;
    /* d_reclen: after the 8-byte inode and 8-byte offset. */
    directory->at += *(unsigned short *)(entry + 16);
    return entry;
}

int closedir(KDirectory *directory) {
    int closed = close(directory->fd);
    kt_unmap(directory, sizeof(KDirectory));
    return closed;
}

int dirfd(KDirectory *directory) { return directory->fd; }

/* ---- stdio --------------------------------------------------------------------------------------

   Enough of it for a program that reads and writes whole files and lines: `FILE` is the layer's
   own and opaque to its callers, as glibc's is in practice. Writes go straight to the descriptor,
   so `setvbuf` and `fflush` have nothing to do; reads are buffered so that `fgets` need not read a
   byte at a time. */

#define KT_FILE_BUFFER 4096

typedef struct {
    int fd;
    bool end;
    bool failed;
    bool owned;
    size_t at;
    size_t filled;
    char buffer[KT_FILE_BUFFER];
} KFile;

static KFile kt_stdin_file = {.fd = 0};
static KFile kt_stdout_file = {.fd = 1};
static KFile kt_stderr_file = {.fd = 2};

KFile *stdin = &kt_stdin_file;
KFile *stdout = &kt_stdout_file;
KFile *stderr = &kt_stderr_file;

/* `fopen`'s mode as `open`'s flags, or -1 for a mode it does not have. */
static int kt_file_flags(const char *mode) {
    int flags;
    switch (mode[0]) {
    case 'r':
        flags = KT_O_RDONLY;
        break;
    case 'w':
        flags = KT_O_WRONLY | KT_O_CREAT | KT_O_TRUNC;
        break;
    case 'a':
        flags = KT_O_WRONLY | KT_O_CREAT | KT_O_APPEND;
        break;
    default:
        return -1;
    }
    for (const char *at = mode + 1; *at != '\0'; at++) {
        if (*at == '+') {
            flags = (flags & ~(KT_O_WRONLY | KT_O_RDONLY)) | KT_O_RDWR;
        } else if (*at == 'e') {
            flags |= KT_O_CLOEXEC;
        }
    }
    return flags;
}

KFile *fdopen(int fd, const char *mode) {
    if (kt_file_flags(mode) < 0) {
        *__errno_location() = KT_EINVAL;
        return NULL;
    }
    KFile *file = (KFile *)kt_map(sizeof(KFile));
    file->fd = fd;
    file->owned = true;
    return file;
}

KFile *fopen(const char *path, const char *mode) {
    int flags = kt_file_flags(mode);
    if (flags < 0) {
        *__errno_location() = KT_EINVAL;
        return NULL;
    }
    int fd = open(path, flags, 0666);
    if (fd < 0) {
        return NULL;
    }
    return fdopen(fd, mode);
}

int fclose(KFile *file) {
    int closed = close(file->fd);
    if (file->owned) {
        kt_unmap(file, sizeof(KFile));
    }
    return closed == 0 ? 0 : -1;
}

int fileno(KFile *file) { return file->fd; }

int feof(KFile *file) { return file->end; }

int ferror(KFile *file) { return file->failed; }

void clearerr(KFile *file) {
    file->end = false;
    file->failed = false;
}

int fflush(KFile *file) {
    (void)file;
    return 0;
}

int setvbuf(KFile *file, char *buffer, int mode, size_t size) {
    (void)file;
    (void)buffer;
    (void)mode;
    (void)size;
    return 0;
}

/* Refill an empty read buffer; false at the end or on a failure, which it records. */
static bool kt_file_fill(KFile *file) {
    if (file->at < file->filled) {
        return true;
    }
    long got;
    do {
        got = kt_syscall(KT_SYS_READ, file->fd, (long)file->buffer, KT_FILE_BUFFER, 0, 0, 0);
    } while (got == -KT_EINTR);
    if (got <= 0) {
        if (got == 0) {
            file->end = true;
        } else {
            file->failed = true;
            kt_posix_result(got);
        }
        return false;
    }
    file->at = 0;
    file->filled = (size_t)got;
    return true;
}

size_t fread(void *into, size_t size, size_t count, KFile *file) {
    size_t wanted = size * count;
    if (wanted == 0) {
        return 0;
    }
    char *bytes = (char *)into;
    size_t got = 0;
    while (got < wanted && kt_file_fill(file)) {
        size_t step = file->filled - file->at;
        if (step > wanted - got) {
            step = wanted - got;
        }
        memcpy(bytes + got, file->buffer + file->at, step);
        file->at += step;
        got += step;
    }
    return got / size;
}

size_t fwrite(const void *from, size_t size, size_t count, KFile *file) {
    size_t wanted = size * count;
    if (wanted == 0) {
        return 0;
    }
    const char *bytes = (const char *)from;
    size_t written = 0;
    while (written < wanted) {
        long step = kt_syscall(KT_SYS_WRITE, file->fd, (long)(bytes + written),
                               (long)(wanted - written), 0, 0, 0);
        if (step == -KT_EINTR) {
            continue;
        }
        if (step <= 0) {
            file->failed = true;
            kt_posix_result(step);
            break;
        }
        written += (size_t)step;
    }
    return written / size;
}

int fputs(const char *text, KFile *file) {
    size_t length = 0;
    while (text[length] != '\0') {
        length++;
    }
    return fwrite(text, 1, length, file) == length ? 0 : -1;
}

int fputc(int character, KFile *file) {
    unsigned char byte = (unsigned char)character;
    return fwrite(&byte, 1, 1, file) == 1 ? byte : -1;
}

int putc(int character, KFile *file) { return fputc(character, file); }

int putchar(int character) { return fputc(character, stdout); }

int puts(const char *text) { return fputs(text, stdout) == 0 && fputc('\n', stdout) >= 0 ? 0 : -1; }

int fgetc(KFile *file) {
    if (!kt_file_fill(file)) {
        return -1;
    }
    return (unsigned char)file->buffer[file->at++];
}

int getc(KFile *file) { return fgetc(file); }

int getchar(void) { return fgetc(stdin); }

char *fgets(char *into, int size, KFile *file) {
    if (size <= 0) {
        return NULL;
    }
    int length = 0;
    while (length < size - 1) {
        int character = fgetc(file);
        if (character < 0) {
            break;
        }
        into[length++] = (char)character;
        if (character == '\n') {
            break;
        }
    }
    if (length == 0) {
        return NULL;
    }
    into[length] = '\0';
    return into;
}
