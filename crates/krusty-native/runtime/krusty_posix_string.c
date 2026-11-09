/* krusty native runtime, POSIX layer: <string.h> beyond the `memcpy` and `memset` the runtime
   itself needs. Byte at a time: compiling fast and staying portable come before these running
   fast. Hand-written freestanding C; see krusty_posix.h. */
#include "krusty_posix.h"

size_t strlen(const char *text) {
    size_t length = 0;
    while (text[length] != '\0') {
        length++;
    }
    return length;
}

size_t strnlen(const char *text, size_t limit) {
    size_t length = 0;
    while (length < limit && text[length] != '\0') {
        length++;
    }
    return length;
}

int memcmp(const void *left, const void *right, size_t length) {
    const unsigned char *a = (const unsigned char *)left;
    const unsigned char *b = (const unsigned char *)right;
    for (size_t i = 0; i < length; i++) {
        if (a[i] != b[i]) {
            return a[i] - b[i];
        }
    }
    return 0;
}

void *memmove(void *destination, const void *source, size_t length) {
    unsigned char *to = (unsigned char *)destination;
    const unsigned char *from = (const unsigned char *)source;
    /* The direction is chosen on the addresses as integers: `<` between two pointers is defined only
       within one object, and the two here are as often two allocations. */
    if ((uintptr_t)to - (uintptr_t)from >= length) {
        for (size_t i = 0; i < length; i++) {
            to[i] = from[i];
        }
    } else {
        for (size_t i = length; i > 0; i--) {
            to[i - 1] = from[i - 1];
        }
    }
    return destination;
}

void *memchr(const void *bytes, int value, size_t length) {
    const unsigned char *at = (const unsigned char *)bytes;
    for (size_t i = 0; i < length; i++) {
        if (at[i] == (unsigned char)value) {
            return (void *)(at + i);
        }
    }
    return NULL;
}

int strcmp(const char *left, const char *right) {
    while (*left != '\0' && *left == *right) {
        left++;
        right++;
    }
    return (unsigned char)*left - (unsigned char)*right;
}

int strncmp(const char *left, const char *right, size_t limit) {
    for (size_t i = 0; i < limit; i++) {
        if (left[i] != right[i] || left[i] == '\0') {
            return (unsigned char)left[i] - (unsigned char)right[i];
        }
    }
    return 0;
}

char *strchr(const char *text, int character) {
    for (;; text++) {
        if (*text == (char)character) {
            return (char *)text;
        }
        if (*text == '\0') {
            return NULL;
        }
    }
}

char *strrchr(const char *text, int character) {
    const char *found = NULL;
    for (;; text++) {
        if (*text == (char)character) {
            found = text;
        }
        if (*text == '\0') {
            return (char *)found;
        }
    }
}

char *strstr(const char *text, const char *wanted) {
    size_t length = strlen(wanted);
    for (; *text != '\0'; text++) {
        if (strncmp(text, wanted, length) == 0) {
            return (char *)text;
        }
    }
    return length == 0 ? (char *)text : NULL;
}

char *strcpy(char *destination, const char *source) {
    memcpy(destination, source, strlen(source) + 1);
    return destination;
}

char *strncpy(char *destination, const char *source, size_t limit) {
    size_t length = strnlen(source, limit);
    memcpy(destination, source, length);
    memset(destination + length, 0, limit - length);
    return destination;
}

char *strcat(char *destination, const char *source) {
    strcpy(destination + strlen(destination), source);
    return destination;
}
