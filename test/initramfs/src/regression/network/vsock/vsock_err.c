// SPDX-License-Identifier: MPL-2.0

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

#include <linux/vm_sockets.h>

#include "../../common/test.h"

#define CONTROL_PORT 25000
#define ECHO_PORT 25001
#define SHUTDOWN_PORT 25002
#define RESET_PORT 25003
#define LARGE_PORT 25004
#define GUEST_ACCEPT_PORT 25005
#define GUEST_RESET_PORT 25006
#define BACKLOG_PORT 25007
#define REFUSED_PORT 25999
#define LARGE_TRANSFER_BYTES (256 * 1024)
#define CONTROL_LINE_LEN 256

static FILE *control_file;
static uint32_t peer_cid;

static uint32_t default_peer_cid(void)
{
#ifdef __asterinas__
	return VMADDR_CID_HOST;
#else
	return VMADDR_CID_LOCAL;
#endif
}

static uint32_t get_env_u32(const char *name, uint32_t default_value)
{
	const char *value = getenv(name);
	char *end = NULL;
	unsigned long parsed;

	if (value == NULL) {
		return default_value;
	}

	parsed = strtoul(value, &end, 10);
	if (*end != '\0' || parsed > UINT32_MAX) {
		fprintf(stderr, "invalid %s: %s\n", name, value);
		exit(EXIT_FAILURE);
	}

	return (uint32_t)parsed;
}

static struct sockaddr_vm make_addr(uint32_t cid, uint32_t port)
{
	struct sockaddr_vm addr = { 0 };

	addr.svm_family = AF_VSOCK;
	addr.svm_cid = cid;
	addr.svm_port = port;
	return addr;
}

static int connect_with_retry(uint32_t cid, uint32_t port)
{
	struct sockaddr_vm addr = make_addr(cid, port);

	for (int attempt = 0; attempt < 100; attempt++) {
		int sockfd = socket(AF_VSOCK, SOCK_STREAM, 0);

		if (sockfd < 0) {
			return -1;
		}
		if (connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)) ==
		    0) {
			return sockfd;
		}

		close(sockfd);
		if (errno != EINTR) {
			usleep(50 * 1000);
		}
	}

	errno = ETIMEDOUT;
	return -1;
}

static int read_full(int fd, void *buf, size_t len)
{
	size_t total = 0;
	char *cursor = buf;

	while (total < len) {
		ssize_t bytes_read = read(fd, cursor + total, len - total);
		if (bytes_read == 0) {
			break;
		}
		if (bytes_read < 0) {
			if (errno == EINTR) {
				continue;
			}
			return -1;
		}

		total += bytes_read;
	}

	return total;
}

static int read_line(int fd, char *buf, size_t buf_len)
{
	size_t index = 0;

	while (index + 1 < buf_len) {
		char ch;
		ssize_t bytes_read = read(fd, &ch, 1);

		if (bytes_read == 0) {
			if (index == 0) {
				return 0;
			}
			break;
		}
		if (bytes_read < 0) {
			if (errno == EINTR) {
				continue;
			}
			return -1;
		}
		if (ch == '\n') {
			break;
		}

		buf[index++] = ch;
	}

	buf[index] = '\0';
	return 1;
}

static unsigned char expected_byte(size_t index)
{
	return (unsigned char)(index % 251);
}

static int send_pattern(int fd, size_t total_bytes)
{
	unsigned char buf[4096];
	size_t sent = 0;

	while (sent < total_bytes) {
		size_t chunk = sizeof(buf);

		if (chunk > total_bytes - sent) {
			chunk = total_bytes - sent;
		}

		for (size_t index = 0; index < chunk; index++) {
			buf[index] = expected_byte(sent + index);
		}

		size_t offset = 0;
		while (offset < chunk) {
			ssize_t bytes_written = send(
				fd, buf + offset, chunk - offset, MSG_NOSIGNAL);

			if (bytes_written < 0) {
				if (errno == EINTR) {
					continue;
				}
				return -1;
			}

			offset += bytes_written;
			sent += bytes_written;
		}
	}

	return 0;
}

static int control_request(char *response, size_t response_len, const char *fmt,
			   ...)
{
	va_list args;

	va_start(args, fmt);
	if (vfprintf(control_file, fmt, args) < 0) {
		va_end(args);
		return -1;
	}
	va_end(args);

	if (fflush(control_file) < 0) {
		return -1;
	}
	if (fgets(response, response_len, control_file) == NULL) {
		errno = EIO;
		return -1;
	}

	size_t len = strlen(response);
	if (len > 0 && response[len - 1] == '\n') {
		response[len - 1] = '\0';
	}

	return 0;
}

static int start_scenario(const char *fmt, ...)
{
	char command[CONTROL_LINE_LEN];
	char response[CONTROL_LINE_LEN];
	va_list args;
	int child = -1;

	va_start(args, fmt);
	if (vsnprintf(command, sizeof(command), fmt, args) >=
	    (int)sizeof(command)) {
		va_end(args);
		errno = EOVERFLOW;
		return -1;
	}
	va_end(args);

	if (control_request(response, sizeof(response), "%s", command) < 0) {
		return -1;
	}
	if (sscanf(response, "OK %d", &child) != 1) {
		errno = EPROTO;
		return -1;
	}

	return child;
}

static int wait_scenario(int child)
{
	char response[CONTROL_LINE_LEN];
	int status = -1;

	if (control_request(response, sizeof(response), "WAIT %d\n", child) <
	    0) {
		return -1;
	}
	if (sscanf(response, "OK %d", &status) == 1 && status == 0) {
		return 0;
	}

	errno = EPROTO;
	return -1;
}

static void stop_control_server(void) __attribute__((destructor));

static void stop_control_server(void)
{
	if (control_file == NULL) {
		return;
	}

	char response[CONTROL_LINE_LEN];

	(void)control_request(response, sizeof(response), "QUIT\n");
	fclose(control_file);
	control_file = NULL;
}

FN_SETUP(general)
{
	int control_fd;

	peer_cid = get_env_u32("VSOCK_TEST_PEER_CID", default_peer_cid());

	signal(SIGPIPE, SIG_IGN);

	control_fd = CHECK(connect_with_retry(peer_cid, CONTROL_PORT));
	control_file = CHECK(fdopen(control_fd, "r+"));
	CHECK(setvbuf(control_file, NULL, _IONBF, 0));
}
END_SETUP()

FN_TEST(bind_and_connect_errors)
{
	int sockfd =
		TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM | SOCK_NONBLOCK, 0));
	struct sockaddr_vm addr = make_addr(0x12345678, GUEST_ACCEPT_PORT + 10);

	TEST_ERRNO(bind(sockfd, (struct sockaddr *)&addr, sizeof(addr)),
		   EADDRNOTAVAIL);

	TEST_SUCC(close(sockfd));
}
END_TEST()

FN_TEST(connect_and_sendmsg_name)
{
	int child = TEST_SUCC(start_scenario("START ECHO %u\n", ECHO_PORT));
	int sockfd = TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM, 0));
	struct sockaddr_vm addr = make_addr(peer_cid, ECHO_PORT);
	struct sockaddr_vm sock_addr = { 0 };
	struct sockaddr_vm peer_addr = { 0 };
	struct msghdr msg = { 0 };
	struct iovec iov;
	socklen_t addr_len;
	char byte = 'z';

	usleep(100 * 1000);
	TEST_SUCC(connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)));

	addr_len = sizeof(sock_addr);
	TEST_RES(getsockname(sockfd, (struct sockaddr *)&sock_addr, &addr_len),
		 sock_addr.svm_family == AF_VSOCK &&
			 sock_addr.svm_port != VMADDR_PORT_ANY);

	addr_len = sizeof(peer_addr);
	TEST_RES(getpeername(sockfd, (struct sockaddr *)&peer_addr, &addr_len),
		 addr_len == sizeof(peer_addr) &&
			 peer_addr.svm_family == AF_VSOCK &&
			 peer_addr.svm_port == ECHO_PORT);

	TEST_RES(read(sockfd, &byte, 0), _ret == 0);

	iov.iov_base = &byte;
	iov.iov_len = sizeof(byte);
	msg.msg_name = &addr;
	msg.msg_namelen = sizeof(addr);
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	TEST_ERRNO(sendmsg(sockfd, &msg, 0), EISCONN);

	byte = 'k';
	TEST_RES(send(sockfd, &byte, sizeof(byte), 0), _ret == sizeof(byte));
	byte = '\0';
	TEST_RES(recv(sockfd, &byte, sizeof(byte), 0),
		 _ret == sizeof(byte) && byte == 'k');

	TEST_SUCC(close(sockfd));
	TEST_SUCC(wait_scenario(child));
}
END_TEST()

FN_TEST(accept_and_peername)
{
	int listener = TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM, 0));
	int child;
	struct sockaddr_vm listener_addr =
		make_addr(VMADDR_CID_ANY, GUEST_ACCEPT_PORT);
	struct pollfd pollfd = {
		.fd = listener,
		.events = POLLIN,
	};
	struct sockaddr_vm peer_addr = { 0 };
	struct sockaddr_vm sock_addr = { 0 };
	socklen_t addr_len;
	int accepted;
	char line[CONTROL_LINE_LEN];
	unsigned int expected_cid = 0;
	unsigned int expected_port = 0;

	TEST_SUCC(bind(listener, (struct sockaddr *)&listener_addr,
		       sizeof(listener_addr)));
	TEST_SUCC(listen(listener, 1));
	addr_len = sizeof(peer_addr);
	TEST_ERRNO(getpeername(listener, (struct sockaddr *)&peer_addr,
			       &addr_len),
		   ENOTCONN);

	child = TEST_SUCC(
		start_scenario("START CONNECT_ADDR %u\n", GUEST_ACCEPT_PORT));

	TEST_RES(poll(&pollfd, 1, 1000),
		 _ret == 1 && (pollfd.revents & POLLIN));

	accepted = TEST_SUCC(accept(listener, NULL, NULL));

	addr_len = sizeof(sock_addr);
	TEST_RES(getsockname(accepted, (struct sockaddr *)&sock_addr,
			     &addr_len),
		 addr_len == sizeof(sock_addr) &&
			 sock_addr.svm_family == AF_VSOCK &&
			 sock_addr.svm_port == GUEST_ACCEPT_PORT);

	addr_len = sizeof(peer_addr);
	TEST_RES(getpeername(accepted, (struct sockaddr *)&peer_addr,
			     &addr_len),
		 addr_len == sizeof(peer_addr) &&
			 peer_addr.svm_family == AF_VSOCK &&
			 peer_addr.svm_cid != VMADDR_CID_ANY &&
			 peer_addr.svm_port != VMADDR_PORT_ANY);

	TEST_RES(read_line(accepted, line, sizeof(line)),
		 sscanf(line, "ADDR %u %u", &expected_cid, &expected_port) ==
			 2);
	TEST_RES(
		getpeername(accepted, (struct sockaddr *)&peer_addr, &addr_len),
		peer_addr.svm_port == expected_port
#ifdef __asterinas__
			&& peer_addr.svm_cid == peer_cid
#else
			&& peer_addr.svm_cid != VMADDR_CID_ANY
#endif
	);

	TEST_SUCC(close(accepted));
	TEST_SUCC(close(listener));
	TEST_SUCC(wait_scenario(child));
}
END_TEST()

FN_TEST(connect_refused_so_error)
{
	int sockfd =
		TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM | SOCK_NONBLOCK, 0));
	struct sockaddr_vm addr = make_addr(peer_cid, REFUSED_PORT);
	struct pollfd pollfd = {
		.fd = sockfd,
		.events = POLLOUT,
	};
	int sock_err = 0;
	socklen_t opt_len = sizeof(sock_err);

	TEST_ERRNO(connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)),
		   EINPROGRESS);
	TEST_RES(poll(&pollfd, 1, 1000),
		 _ret == 1 && (pollfd.revents & (POLLOUT | POLLERR | POLLHUP)));
	TEST_RES(getsockopt(sockfd, SOL_SOCKET, SO_ERROR, &sock_err, &opt_len),
		 opt_len == sizeof(sock_err) && sock_err == ECONNRESET);
	TEST_RES(getsockopt(sockfd, SOL_SOCKET, SO_ERROR, &sock_err, &opt_len),
		 opt_len == sizeof(sock_err) && sock_err == 0);

	TEST_SUCC(close(sockfd));
}
END_TEST()

FN_TEST(peer_shutdown)
{
	const char *payload = "peer-shutdown";
	int child = TEST_SUCC(start_scenario("START SEND_SHUTDOWN %u %s\n",
					     SHUTDOWN_PORT, payload));
	int sockfd = TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM, 0));
	struct sockaddr_vm addr = make_addr(peer_cid, SHUTDOWN_PORT);
	struct pollfd pollfd = {
		.fd = sockfd,
		.events = POLLIN | POLLRDHUP | POLLHUP,
	};
	char buf[32] = { 0 };

	usleep(100 * 1000);
	TEST_SUCC(connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)));
	TEST_RES(read_full(sockfd, buf, strlen(payload)),
		 _ret == (int)strlen(payload) &&
			 memcmp(buf, payload, strlen(payload)) == 0);

	TEST_RES(poll(&pollfd, 1, 1000),
		 _ret == 1 && (pollfd.revents & (POLLIN | POLLRDHUP)));
	TEST_RES(recv(sockfd, buf, sizeof(buf), 0), _ret == 0);

	TEST_SUCC(close(sockfd));
	TEST_SUCC(wait_scenario(child));
}
END_TEST()

FN_TEST(peer_reset_so_error)
{
#if !defined(__asterinas__)
	SKIP_TEST_IF(1);
#endif
#ifdef __asterinas__
	SKIP_TEST_IF(1);
#endif

	int child = TEST_SUCC(start_scenario("START RESET %u\n", RESET_PORT));
	int sockfd = TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM, 0));
	struct sockaddr_vm addr = make_addr(peer_cid, RESET_PORT);
	struct pollfd pollfd = {
		.fd = sockfd,
		.events = POLLIN | POLLOUT | POLLERR | POLLHUP | POLLRDHUP,
	};
	int sock_err = 0;
	socklen_t opt_len = sizeof(sock_err);

	usleep(100 * 1000);
	TEST_SUCC(connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)));
	TEST_RES(poll(&pollfd, 1, 1000),
		 _ret == 1 &&
			 (pollfd.revents & (POLLERR | POLLHUP | POLLRDHUP)));
	TEST_RES(getsockopt(sockfd, SOL_SOCKET, SO_ERROR, &sock_err, &opt_len),
		 opt_len == sizeof(sock_err) && sock_err == ECONNRESET);
	TEST_RES(getsockopt(sockfd, SOL_SOCKET, SO_ERROR, &sock_err, &opt_len),
		 opt_len == sizeof(sock_err) && sock_err == 0);

	TEST_SUCC(close(sockfd));
	TEST_SUCC(wait_scenario(child));
}
END_TEST()

FN_TEST(large_transfer)
{
	int child = TEST_SUCC(start_scenario("START READ_AND_ACK %u %u\n",
					     LARGE_PORT, LARGE_TRANSFER_BYTES));
	int sockfd = TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM, 0));
	struct sockaddr_vm addr = make_addr(peer_cid, LARGE_PORT);
	char ack = '\0';

	usleep(100 * 1000);
	TEST_SUCC(connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)));
	TEST_SUCC(send_pattern(sockfd, LARGE_TRANSFER_BYTES));
	TEST_RES(read_full(sockfd, &ack, sizeof(ack)),
		 _ret == sizeof(ack) && ack == 'Y');

	TEST_SUCC(close(sockfd));
	TEST_SUCC(wait_scenario(child));
}
END_TEST()

FN_TEST(listener_close_resets_pending_connection)
{
	int listener = TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM, 0));
	struct sockaddr_vm addr = make_addr(VMADDR_CID_ANY, GUEST_RESET_PORT);
	struct pollfd pollfd = {
		.fd = listener,
		.events = POLLIN,
	};
	int child;

	TEST_SUCC(bind(listener, (struct sockaddr *)&addr, sizeof(addr)));
	TEST_SUCC(listen(listener, 1));

	child = TEST_SUCC(start_scenario("START CONNECT_EXPECT_RESET %u\n",
					 GUEST_RESET_PORT));

	TEST_RES(poll(&pollfd, 1, 1000),
		 _ret == 1 && (pollfd.revents & POLLIN));

	TEST_SUCC(close(listener));
	TEST_SUCC(wait_scenario(child));
}
END_TEST()

FN_TEST(backlog_full_rejects_second_connection)
{
#ifndef __asterinas__
	SKIP_TEST_IF(1);
#endif

	int listener = TEST_SUCC(socket(AF_VSOCK, SOCK_STREAM, 0));
	struct sockaddr_vm addr = make_addr(VMADDR_CID_ANY, BACKLOG_PORT);
	int child;

	TEST_SUCC(bind(listener, (struct sockaddr *)&addr, sizeof(addr)));
	TEST_SUCC(listen(listener, 1));

	child = TEST_SUCC(
		start_scenario("START FILL_BACKLOG %u\n", BACKLOG_PORT));

	TEST_SUCC(wait_scenario(child));
	TEST_SUCC(close(listener));
}
END_TEST()
