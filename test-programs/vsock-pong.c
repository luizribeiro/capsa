// vsock-pong: A simple ping-pong responder over vsock
//
// Usage: vsock-pong <port>
//
// Connects to the host (CID 2) on the specified port via vsock,
// then enters a loop: reads data, if it's "ping", responds with "pong".
//
// TODO: Migrate to Rust for consistency with the rest of the codebase.
// Use x86_64-unknown-linux-musl target for static linking (no glibc needed).
// With optimizations (opt-level="z", lto=true, strip=true, panic="abort"),
// expect ~100-150KB binary size. Use `vsock` or `nix` crate for vsock support.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <linux/vm_sockets.h>

#define HOST_CID 2
#define BUFFER_SIZE 256

int main(int argc, char *argv[]) {
    if (argc != 2) {
        fprintf(stderr, "Usage: %s <port>\n", argv[0]);
        return 1;
    }

    int port = atoi(argv[1]);
    if (port <= 0 || port > 65535) {
        fprintf(stderr, "Invalid port: %s\n", argv[1]);
        return 1;
    }

    printf("vsock-pong: connecting to host CID %d port %d\n", HOST_CID, port);

    int sock = socket(AF_VSOCK, SOCK_STREAM, 0);
    if (sock < 0) {
        perror("socket");
        return 1;
    }

    struct sockaddr_vm addr = {
        .svm_family = AF_VSOCK,
        .svm_cid = HOST_CID,
        .svm_port = port,
    };

    if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("connect");
        close(sock);
        return 1;
    }

    printf("vsock-pong: connected!\n");

    char buffer[BUFFER_SIZE];
    ssize_t n;

    while ((n = read(sock, buffer, sizeof(buffer) - 1)) > 0) {
        buffer[n] = '\0';

        // Strip trailing newline if present
        while (n > 0 && (buffer[n-1] == '\n' || buffer[n-1] == '\r')) {
            buffer[--n] = '\0';
        }

        printf("vsock-pong: received '%s'\n", buffer);

        if (strcmp(buffer, "ping") == 0) {
            const char *response = "pong";
            if (write(sock, response, strlen(response)) < 0) {
                perror("write");
                break;
            }
            printf("vsock-pong: sent 'pong'\n");
        } else if (strcmp(buffer, "quit") == 0) {
            printf("vsock-pong: received quit, exiting\n");
            break;
        } else {
            // Echo unknown messages back
            if (write(sock, buffer, n) < 0) {
                perror("write");
                break;
            }
        }
    }

    if (n < 0) {
        perror("read");
    }

    close(sock);
    printf("vsock-pong: connection closed\n");
    return 0;
}
