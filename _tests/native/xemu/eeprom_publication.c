/* Link the real generator and cipher objects; control only entropy and scheduling. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <glib.h>

void *error_fatal;
int qcrypto_random_bytes(void *buf, size_t len, void **errp)
{
    (void)errp;
    memset(buf, 0x5a, len);
    return 0;
}
extern bool xbox_eeprom_generate(const char *, int);

/* The old fopen path exposes the destination before writing. Observe exactly
 * that permitted interleaving, without depending on thread scheduling. */
FILE *qemu_fopen(const char *path, const char *mode)
{
    FILE *f = fopen(path, mode);
    if (f) {
        struct stat st;
        assert(stat(path, &st) == 0);
        assert(st.st_size == 256 && "incomplete EEPROM became visible");
    }
    return f;
}

static const char *directory;
static gpointer reader(gpointer unused)
{
    (void)unused;
    for (int i = 0; i < 500; ++i) {
        char *path = g_strdup_printf("%s/%d.bin", directory, i);
        FILE *f;
        while (!(f = fopen(path, "rb"))) g_thread_yield();
        unsigned char bytes[257];
        assert(fread(bytes, 1, sizeof(bytes), f) == 256);
        assert(feof(f));
        assert(memcmp(bytes + 52, "000000000000", 12) == 0); /* Fixture serial. */
        fclose(f);
        g_free(path);
    }
    return NULL;
}
int main(int argc, char **argv)
{
    assert(argc == 2);
    directory = argv[1];
    GThread *t = g_thread_new("reader", reader, NULL);
    for (int i = 0; i < 500; ++i) {
        char *path = g_strdup_printf("%s/%d.bin", directory, i);
        assert(xbox_eeprom_generate(path, 1));
        g_free(path);
    }
    g_thread_join(t);
    char *bad = g_strdup_printf("%s/missing/eeprom.bin", directory);
    assert(!xbox_eeprom_generate(bad, 1));
    assert(!g_file_test(bad, G_FILE_TEST_EXISTS));
    g_free(bad);
    assert(!xbox_eeprom_generate(directory, 1));
    puts("500 concurrent publications complete; missing parent and directory destination rejected");
    return 0;
}
