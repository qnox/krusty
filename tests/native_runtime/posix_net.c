/* Sockets and epoll through the POSIX layer, as a non-blocking server uses them: a loopback
   listener, EAGAIN before a peer arrives, readiness through epoll in glibc's `struct epoll_event`
   (packed on x86_64), socket options, and the end of a connection. */
#include "driver_posix.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/epoll.h>
#include <sys/socket.h>

static int wait_for(int poll, int expected_fd, uint32_t expected_events) {
    struct epoll_event events[4];
    int ready = epoll_wait(poll, events, 4, 5000);
    DRIVER_CHECK(ready == 1, "epoll_wait did not report exactly one descriptor");
    DRIVER_CHECK(events[0].data.fd == expected_fd, "epoll_wait reported another descriptor");
    DRIVER_CHECK((events[0].events & expected_events) == expected_events,
                 "epoll_wait reported other events");
    return ready;
}

void kt_program_entry(void) {
    int listener = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    DRIVER_CHECK(listener >= 0, "socket failed");
    int on = 1;
    DRIVER_CHECK(setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &on, sizeof on) == 0 &&
                     setsockopt(listener, SOL_SOCKET, SO_REUSEPORT, &on, sizeof on) == 0,
                 "setsockopt on the listener failed");
    struct sockaddr_in address;
    memset(&address, 0, sizeof address);
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    DRIVER_CHECK(bind(listener, (struct sockaddr *)&address, sizeof address) == 0, "bind failed");
    DRIVER_CHECK(listen(listener, 16) == 0, "listen failed");
    socklen_t length = sizeof address;
    DRIVER_CHECK(getsockname(listener, (struct sockaddr *)&address, &length) == 0 &&
                     length == sizeof address && address.sin_port != 0,
                 "getsockname did not answer the bound port");

    DRIVER_CHECK(accept4(listener, NULL, NULL, SOCK_NONBLOCK) == -1 && errno == EAGAIN,
                 "accepting with nobody waiting is not EAGAIN");

    int poll = epoll_create1(EPOLL_CLOEXEC);
    DRIVER_CHECK(poll >= 0, "epoll_create1 failed");
    struct epoll_event interest = {.events = EPOLLIN, .data.fd = listener};
    DRIVER_CHECK(epoll_ctl(poll, EPOLL_CTL_ADD, listener, &interest) == 0, "epoll_ctl failed");
    DRIVER_CHECK(epoll_wait(poll, &interest, 1, 0) == 0, "epoll_wait reported an idle listener");

    int client = socket(AF_INET, SOCK_STREAM, 0);
    DRIVER_CHECK(client >= 0, "the client socket failed");
    DRIVER_CHECK(connect(client, (struct sockaddr *)&address, sizeof address) == 0,
                 "connect failed");
    wait_for(poll, listener, EPOLLIN);

    struct sockaddr_in peer;
    socklen_t peer_length = sizeof peer;
    int server = accept4(listener, (struct sockaddr *)&peer, &peer_length, SOCK_NONBLOCK);
    DRIVER_CHECK(server >= 0 && peer.sin_family == AF_INET &&
                     peer.sin_addr.s_addr == htonl(INADDR_LOOPBACK),
                 "accept4 did not answer the client's address");
    DRIVER_CHECK(setsockopt(server, IPPROTO_TCP, TCP_NODELAY, &on, sizeof on) == 0,
                 "TCP_NODELAY failed");
    int value = 0;
    socklen_t value_length = sizeof value;
    DRIVER_CHECK(getsockopt(server, IPPROTO_TCP, TCP_NODELAY, &value, &value_length) == 0 &&
                     value != 0,
                 "TCP_NODELAY did not read back");

    struct epoll_event readable = {.events = EPOLLIN | EPOLLRDHUP, .data.fd = server};
    DRIVER_CHECK(epoll_ctl(poll, EPOLL_CTL_DEL, listener, NULL) == 0 &&
                     epoll_ctl(poll, EPOLL_CTL_ADD, server, &readable) == 0,
                 "re-registering with epoll failed");
    char bytes[16];
    DRIVER_CHECK(recv(server, bytes, sizeof bytes, 0) == -1 && errno == EAGAIN,
                 "reading an idle connection is not EAGAIN");

    DRIVER_CHECK(send(client, "ping", 4, MSG_NOSIGNAL) == 4, "send failed");
    wait_for(poll, server, EPOLLIN);
    DRIVER_CHECK(recv(server, bytes, sizeof bytes, 0) == 4 && memcmp(bytes, "ping", 4) == 0,
                 "recv did not get what was sent");
    DRIVER_CHECK(sendto(server, "pong", 4, MSG_NOSIGNAL, NULL, 0) == 4, "sendto failed");
    DRIVER_CHECK(recvfrom(client, bytes, sizeof bytes, 0, NULL, NULL) == 4 &&
                     memcmp(bytes, "pong", 4) == 0,
                 "recvfrom did not get the reply");

    DRIVER_CHECK(shutdown(client, SHUT_WR) == 0, "shutdown failed");
    wait_for(poll, server, EPOLLIN | EPOLLRDHUP);
    DRIVER_CHECK(recv(server, bytes, sizeof bytes, 0) == 0, "the peer's end is not end of input");

    DRIVER_CHECK(close(client) == 0 && close(server) == 0 && close(listener) == 0 &&
                     close(poll) == 0,
                 "closing failed");
    driver_say("OK\n");
}
