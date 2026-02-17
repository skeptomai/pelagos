/* Test program to directly call blocked syscalls */
#include <stdio.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <errno.h>
#include <string.h>

int main() {
    printf("Testing direct syscall: reboot()\n");

    /* Directly call reboot syscall (SYS_reboot = 169 on x86_64) */
    long ret = syscall(SYS_reboot, 0, 0, 0, 0);

    if (ret == -1) {
        printf("reboot() failed: %s (errno=%d)\n", strerror(errno), errno);
        if (errno == EPERM) {
            printf("SUCCESS: Seccomp blocked reboot syscall with EPERM\n");
            return 0;
        }
    } else {
        printf("FAIL: reboot() succeeded (returned %ld)\n", ret);
        return 1;
    }

    return 0;
}
