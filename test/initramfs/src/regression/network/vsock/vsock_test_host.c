// SPDX-License-Identifier: MPL-2.0

#define _GNU_SOURCE

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <linux/vm_sockets.h>

#define DEFAULT_CONTROL_PORT 25000
#define CONNECT_RETRY_MS 50
#define CONNECT_TIMEOUT_MS 5000
#define CONTROL_LINE_LEN 256
#define RESET_WAIT_MS 2000

static uint32_t bind_cid = VMADDR_CID_ANY;
static uint32_t connect_cid = VMADDR_CID_LOCAL;
static uint32_t control_port = DEFAULT_CONTROL_PORT;

static void make_addr(struct sockaddr_vm *addr, uint32_t cid, uint32_t port)
{
	memset(addr, 0, sizeof(*addr));
	addr->svm_family = AF_VSOCK;
	addr->svm_cid = cid;
	addr->svm_port = port;
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

static int write_all(int fd, const void *buf, size_t len)
{
	const char *cursor = buf;

	while (len > 0) {
		ssize_t bytes_written = send(fd, cursor, len, MSG_NOSIGNAL);
		if (bytes_written < 0) {
			if (errno == EINTR) {
				continue;
			}
			return -1;
		}

		cursor += bytes_written;
		len -= bytes_written;
	}

	return 0;
}

static int send_reply(int fd, const char *fmt, ...)
{
	char line[CONTROL_LINE_LEN];
	va_list args;
	int len;

	va_start(args, fmt);
	len = vsnprintf(line, sizeof(line), fmt, args);
	va_end(args);
	if (len < 0 || (size_t)len >= sizeof(line)) {
		errno = EOVERFLOW;
		return -1;
	}

	return write_all(fd, line, len);
}

static int new_socket(int flags)
{
	return socket(AF_VSOCK, SOCK_STREAM | flags, 0);
}

static int bind_and_listen(uint32_t port)
{
	int sockfd = new_socket(0);
	struct sockaddr_vm addr;

	if (sockfd < 0) {
		return -1;
	}

	make_addr(&addr, bind_cid, port);
	if (bind(sockfd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
		goto err;
	}
	if (listen(sockfd, 4) < 0) {
		goto err;
	}

	return sockfd;

err:
	close(sockfd);
	return -1;
}

static int accept_one(int listener)
{
	for (;;) {
		int accepted = accept(listener, NULL, NULL);
		if (accepted >= 0) {
			return accepted;
		}
		if (errno != EINTR) {
			return -1;
		}
	}
}

static int connect_with_retry(uint32_t cid, uint32_t port)
{
	struct sockaddr_vm addr;
	int elapsed_ms = 0;

	make_addr(&addr, cid, port);

	while (elapsed_ms <= CONNECT_TIMEOUT_MS) {
		int sockfd = new_socket(0);

		if (sockfd < 0) {
			return -1;
		}

		if (connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)) ==
		    0) {
			return sockfd;
		}

		close(sockfd);
		if (errno != EINTR) {
			usleep(CONNECT_RETRY_MS * 1000);
			elapsed_ms += CONNECT_RETRY_MS;
		}
	}

	errno = ETIMEDOUT;
	return -1;
}

static unsigned char expected_byte(size_t index)
{
	return (unsigned char)(index % 251);
}

static int scenario_echo(uint32_t port)
{
	int listener = bind_and_listen(port);
	if (listener < 0) {
		return -1;
	}

	int sockfd = accept_one(listener);
	if (sockfd < 0) {
		close(listener);
		return -1;
	}

	for (;;) {
		char buf[4096];
		ssize_t bytes_read = read(sockfd, buf, sizeof(buf));

		if (bytes_read == 0) {
			break;
		}
		if (bytes_read < 0) {
			if (errno == EINTR) {
				continue;
			}
			close(sockfd);
			close(listener);
			return -1;
		}
		if (write_all(sockfd, buf, bytes_read) < 0) {
			close(sockfd);
			close(listener);
			return -1;
		}
	}

	close(sockfd);
	close(listener);
	return 0;
}

static int scenario_send_then_shutdown(uint32_t port, const char *payload)
{
	int listener = bind_and_listen(port);
	if (listener < 0) {
		return -1;
	}

	int sockfd = accept_one(listener);
	if (sockfd < 0) {
		close(listener);
		return -1;
	}

	if (write_all(sockfd, payload, strlen(payload)) < 0) {
		close(sockfd);
		close(listener);
		return -1;
	}
	if (shutdown(sockfd, SHUT_WR) < 0) {
		close(sockfd);
		close(listener);
		return -1;
	}

	for (;;) {
		char buf[256];
		ssize_t bytes_read = read(sockfd, buf, sizeof(buf));

		if (bytes_read == 0) {
			break;
		}
		if (bytes_read < 0) {
			if (errno == EINTR) {
				continue;
			}
			if (errno == ECONNRESET) {
				break;
			}
			close(sockfd);
			close(listener);
			return -1;
		}
	}

	close(sockfd);
	close(listener);
	return 0;
}

static int scenario_reset(uint32_t port)
{
	int listener = bind_and_listen(port);
	if (listener < 0) {
		return -1;
	}

	int sockfd = accept_one(listener);
	if (sockfd < 0) {
		close(listener);
		return -1;
	}

	struct linger linger_opt = {
		.l_onoff = 1,
		.l_linger = 0,
	};

	usleep(100 * 1000);
	if (setsockopt(sockfd, SOL_SOCKET, SO_LINGER, &linger_opt,
		       sizeof(linger_opt)) < 0) {
		close(sockfd);
		close(listener);
		return -1;
	}

	close(sockfd);
	close(listener);
	return 0;
}

static int scenario_read_and_ack(uint32_t port, size_t total_bytes)
{
	int listener = bind_and_listen(port);
	if (listener < 0) {
		return -1;
	}

	int sockfd = accept_one(listener);
	if (sockfd < 0) {
		close(listener);
		return -1;
	}

	size_t received = 0;
	while (received < total_bytes) {
		unsigned char buf[4096];
		ssize_t bytes_read = read(sockfd, buf, sizeof(buf));

		if (bytes_read == 0) {
			close(sockfd);
			close(listener);
			errno = ECONNABORTED;
			return -1;
		}
		if (bytes_read < 0) {
			if (errno == EINTR) {
				continue;
			}
			close(sockfd);
			close(listener);
			return -1;
		}

		for (ssize_t index = 0; index < bytes_read; index++) {
			if (buf[index] != expected_byte(received + index)) {
				close(sockfd);
				close(listener);
				errno = EPROTO;
				return -1;
			}
		}

		received += bytes_read;
	}

	if (write_all(sockfd, "Y", 1) < 0) {
		close(sockfd);
		close(listener);
		return -1;
	}

	close(sockfd);
	close(listener);
	return 0;
}

static int scenario_connect_addr(uint32_t port)
{
	int sockfd = connect_with_retry(connect_cid, port);
	if (sockfd < 0) {
		return -1;
	}

	struct sockaddr_vm addr;
	socklen_t addr_len = sizeof(addr);
	char line[CONTROL_LINE_LEN];
	ssize_t line_len;

	if (getsockname(sockfd, (struct sockaddr *)&addr, &addr_len) < 0) {
		close(sockfd);
		return -1;
	}

	line_len = snprintf(line, sizeof(line), "ADDR %u %u\n", addr.svm_cid,
			    addr.svm_port);
	if (line_len < 0 || (size_t)line_len >= sizeof(line)) {
		close(sockfd);
		errno = EOVERFLOW;
		return -1;
	}
	if (write_all(sockfd, line, line_len) < 0) {
		close(sockfd);
		return -1;
	}

	for (;;) {
		char buf[64];
		ssize_t bytes_read = read(sockfd, buf, sizeof(buf));

		if (bytes_read == 0) {
			break;
		}
		if (bytes_read < 0) {
			if (errno == EINTR) {
				continue;
			}
			if (errno == ECONNRESET) {
				break;
			}
			close(sockfd);
			return -1;
		}
	}

	close(sockfd);
	return 0;
}

static bool socket_looks_reset(int sockfd)
{
	struct pollfd pollfd = {
		.fd = sockfd,
		.events = POLLIN | POLLOUT | POLLERR | POLLHUP | POLLRDHUP,
	};

	if (poll(&pollfd, 1, 100) < 0) {
		return false;
	}

	if (pollfd.revents & (POLLERR | POLLHUP | POLLRDHUP)) {
		int sock_err = 0;
		socklen_t len = sizeof(sock_err);

		if (getsockopt(sockfd, SOL_SOCKET, SO_ERROR, &sock_err, &len) ==
			    0 &&
		    sock_err != 0) {
			return true;
		}

		char byte;
		ssize_t bytes_read =
			recv(sockfd, &byte, sizeof(byte), MSG_DONTWAIT);
		if (bytes_read == 0) {
			return true;
		}
		if (bytes_read < 0 && errno != EAGAIN && errno != EWOULDBLOCK) {
			return true;
		}
	}

	if (send(sockfd, "x", 1, MSG_NOSIGNAL) < 0) {
		return errno == ECONNRESET || errno == EPIPE ||
		       errno == ENOTCONN || errno == ECONNABORTED;
	}

	return false;
}

static int scenario_connect_expect_reset(uint32_t port)
{
	int sockfd = connect_with_retry(connect_cid, port);
	int elapsed_ms = 0;

	if (sockfd < 0) {
		return -1;
	}

	while (elapsed_ms <= RESET_WAIT_MS) {
		if (socket_looks_reset(sockfd)) {
			close(sockfd);
			return 0;
		}

		usleep(100 * 1000);
		elapsed_ms += 100;
	}

	close(sockfd);
	errno = ETIMEDOUT;
	return -1;
}

static bool second_connect_failed(uint32_t port)
{
	int sockfd = new_socket(SOCK_NONBLOCK);
	struct sockaddr_vm addr;
	struct pollfd pollfd = {
		.events = POLLOUT | POLLERR | POLLHUP | POLLRDHUP,
	};
	int sock_err = 0;
	socklen_t len = sizeof(sock_err);

	if (sockfd < 0) {
		return false;
	}

	make_addr(&addr, connect_cid, port);
	if (connect(sockfd, (struct sockaddr *)&addr, sizeof(addr)) < 0 &&
	    errno != EINPROGRESS) {
		close(sockfd);
		return true;
	}

	pollfd.fd = sockfd;
	if (poll(&pollfd, 1, 1000) < 0) {
		close(sockfd);
		return false;
	}
	if (getsockopt(sockfd, SOL_SOCKET, SO_ERROR, &sock_err, &len) < 0) {
		close(sockfd);
		return false;
	}

	close(sockfd);
	return sock_err != 0;
}

static int scenario_fill_backlog(uint32_t port)
{
	int first_sockfd = connect_with_retry(connect_cid, port);

	if (first_sockfd < 0) {
		return -1;
	}

	bool failed = second_connect_failed(port);

	close(first_sockfd);
	if (!failed) {
		errno = EPROTO;
		return -1;
	}

	return 0;
}

static int run_child_scenario(const char *line)
{
	uint32_t port;
	uint32_t bytes;
	char payload[CONTROL_LINE_LEN];

	if (sscanf(line, "START ECHO %u", &port) == 1) {
		return scenario_echo(port);
	}
	if (sscanf(line, "START SEND_SHUTDOWN %u %255s", &port, payload) == 2) {
		return scenario_send_then_shutdown(port, payload);
	}
	if (sscanf(line, "START RESET %u", &port) == 1) {
		return scenario_reset(port);
	}
	if (sscanf(line, "START READ_AND_ACK %u %u", &port, &bytes) == 2) {
		return scenario_read_and_ack(port, bytes);
	}
	if (sscanf(line, "START CONNECT_ADDR %u", &port) == 1) {
		return scenario_connect_addr(port);
	}
	if (sscanf(line, "START CONNECT_EXPECT_RESET %u", &port) == 1) {
		return scenario_connect_expect_reset(port);
	}
	if (sscanf(line, "START FILL_BACKLOG %u", &port) == 1) {
		return scenario_fill_backlog(port);
	}

	errno = EINVAL;
	return -1;
}

static int handle_start(int connfd, const char *line)
{
	pid_t child = fork();

	if (child < 0) {
		return send_reply(connfd, "ERR fork %d\n", errno);
	}
	if (child == 0) {
		int ret = run_child_scenario(line);
		_exit(ret == 0 ? 0 : 1);
	}

	return send_reply(connfd, "OK %d\n", child);
}

static int handle_wait(int connfd, pid_t child)
{
	int status;

	if (waitpid(child, &status, 0) < 0) {
		return send_reply(connfd, "ERR wait %d\n", errno);
	}
	if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
		return send_reply(connfd, "OK 0\n");
	}

	return send_reply(connfd, "ERR child %d\n", status);
}

static int serve_control_connection(int connfd)
{
	char line[CONTROL_LINE_LEN];

	for (;;) {
		int read_result = read_line(connfd, line, sizeof(line));
		if (read_result <= 0) {
			return read_result;
		}

		if (strcmp(line, "QUIT") == 0) {
			return send_reply(connfd, "OK 0\n");
		}
		if (strncmp(line, "WAIT ", 5) == 0) {
			long raw_pid = strtol(line + 5, NULL, 10);
			if (raw_pid <= 0) {
				if (send_reply(connfd, "ERR wait %d\n",
					       EINVAL) < 0) {
					return -1;
				}
			} else if (handle_wait(connfd, (pid_t)raw_pid) < 0) {
				return -1;
			}
			continue;
		}
		if (strncmp(line, "START ", 6) == 0) {
			if (handle_start(connfd, line) < 0) {
				return -1;
			}
			continue;
		}

		if (send_reply(connfd, "ERR cmd %d\n", EINVAL) < 0) {
			return -1;
		}
	}
}

static int parse_u32_arg(const char *arg, const char *prefix, uint32_t *value)
{
	char *end = NULL;
	unsigned long parsed;

	if (strncmp(arg, prefix, strlen(prefix)) != 0) {
		return 0;
	}

	parsed = strtoul(arg + strlen(prefix), &end, 10);
	if (*end != '\0' || parsed > UINT32_MAX) {
		return -1;
	}

	*value = (uint32_t)parsed;
	return 1;
}

int main(int argc, char *argv[])
{
	int listener;
	int connfd;

	signal(SIGPIPE, SIG_IGN);

	for (int index = 1; index < argc; index++) {
		int matched;

		matched = parse_u32_arg(argv[index], "--bind-cid=", &bind_cid);
		if (matched != 0) {
			if (matched < 0) {
				return EXIT_FAILURE;
			}
			continue;
		}

		matched = parse_u32_arg(argv[index],
					"--connect-cid=", &connect_cid);
		if (matched != 0) {
			if (matched < 0) {
				return EXIT_FAILURE;
			}
			continue;
		}

		matched = parse_u32_arg(argv[index],
					"--control-port=", &control_port);
		if (matched != 0) {
			if (matched < 0) {
				return EXIT_FAILURE;
			}
			continue;
		}

		fprintf(stderr, "unknown argument: %s\n", argv[index]);
		return EXIT_FAILURE;
	}

	listener = bind_and_listen(control_port);
	if (listener < 0) {
		perror("failed to listen on control port");
		return EXIT_FAILURE;
	}

	connfd = accept_one(listener);
	if (connfd < 0) {
		perror("failed to accept control connection");
		close(listener);
		return EXIT_FAILURE;
	}

	if (serve_control_connection(connfd) < 0) {
		perror("control connection failed");
		close(connfd);
		close(listener);
		return EXIT_FAILURE;
	}

	close(connfd);
	close(listener);
	return EXIT_SUCCESS;
}
