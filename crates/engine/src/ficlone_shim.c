/* FICLONE ioctl shim for fiex.
 *
 * FICLONE is declared as _IOW(0x94, 9, int) in <linux/fs.h>. We forward
 * the call to libc's ioctl() with a fixed third argument so we can bind
 * the symbol from Rust without dealing with C variadics in FFI.
 *
 * If the platform's <linux/fs.h> doesn't define FICLONE (older NDKs,
 * minimal musl libc), we fall back to the documented numeric value so
 * the build still works — the syscall will then return ENOTTY at
 * runtime on kernels/filesystems that don't support reflinks, which the
 * engine handles as a normal "not supported" signal.
 */

#include <sys/ioctl.h>
#include <linux/fs.h>

#ifndef FICLONE
/* _IOW(0x94, 9, int) — kernel ABI, stable since Linux 4.5. */
#define FICLONE 0x40089409
#endif

int fiex_ficlone(int dst_fd, int src_fd) {
    return ioctl(dst_fd, FICLONE, src_fd);
}
