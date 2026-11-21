/* exclusive-rename: atomically rename src to dst, failing if dst already exists (including a dangling symlink).
 * Uses renamex_np(RENAME_EXCL) on Darwin; the kernel guarantees exclusivity — no TOCTOU.
 * Built by package-macos.sh with the system cc at runtime; not shipped as a binary.
 * The #ifdef lets the file compile-check on non-Darwin hosts (where it is never built).
 */
#include <stdio.h>
#include <string.h>

#ifdef __APPLE__
#include <fcntl.h>   /* RENAME_EXCL (Darwin 12+) */
#endif
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: exclusive-rename SRC DST\n");
        return 2;
    }
#ifdef __APPLE__
    if (renamex_np(argv[1], argv[2], RENAME_EXCL) == -1) {
        perror("renamex_np");
        return 1;
    }
#else
    fprintf(stderr, "exclusive-rename: renamex_np is Darwin-only\n");
    return 2;
#endif
    return 0;
}
