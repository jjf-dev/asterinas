// SPDX-License-Identifier: MPL-2.0

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../../common/test.h"

static void make_fifo_path(char *path, size_t size)
{
	snprintf(path, size, "/tmp/proc-fd-reopen-fifo-%d", getpid());
}

FN_TEST(reopen_fifo_via_proc_fd)
{
	char fifo_path[PATH_MAX];
	char proc_fd_path[PATH_MAX];
	char link_target[PATH_MAX];
	int fifo_fd;
	pid_t child;
	int writer_fd;
	int status;
	pid_t waited;
	char buf = 0;

	make_fifo_path(fifo_path, sizeof(fifo_path));
	(void)unlink(fifo_path);

	TEST_SUCC(mkfifo(fifo_path, 0644));

	fifo_fd = TEST_SUCC(open(fifo_path, O_PATH | O_CLOEXEC));
	snprintf(proc_fd_path, sizeof(proc_fd_path), "/proc/self/fd/%d", fifo_fd);

	child = TEST_SUCC(fork());
	if (child == 0) {
		ssize_t len;
		int reopened_fd;
		TEST_SUCC(setresgid(65535, 65535, 65535));
		TEST_SUCC(setresuid(65535, 65535, 65535));

		len = readlink(proc_fd_path, link_target, sizeof(link_target) - 1);
		if (len < 0) {
			_exit(10);
		}
		link_target[len] = '\0';
		if (strcmp(link_target, fifo_path) != 0) {
			_exit(11);
		}

		reopened_fd = open(proc_fd_path, O_RDONLY | O_CLOEXEC);
		if (reopened_fd < 0) {
			_exit(12);
		}

		if (read(reopened_fd, &buf, 1) != 1) {
			close(reopened_fd);
			_exit(13);
		}

		close(reopened_fd);
		_exit(buf == 'X' ? 0 : 14);
	}

	usleep(100 * 1000);
	waited = waitpid(child, &status, WNOHANG);
	if (waited == child) {
		__tests_failed++;
		if (WIFEXITED(status)) {
			fprintf(stderr,
				"%s: child exited before writer open [exit=%d]\n",
				__func__, WEXITSTATUS(status));
		} else if (WIFSIGNALED(status)) {
			fprintf(stderr,
				"%s: child exited before writer open [signal=%d]\n",
				__func__, WTERMSIG(status));
		} else {
			fprintf(stderr,
				"%s: child exited before writer open [status=%d]\n",
				__func__, status);
		}
		TEST_SUCC(close(fifo_fd));
		TEST_SUCC(unlink(fifo_path));
		return;
	}
	TEST_RES(waited, _ret == 0);

	writer_fd = open(fifo_path, O_WRONLY | O_CLOEXEC | O_NONBLOCK);
	if (writer_fd < 0) {
		int open_errno = errno;
		TEST_RES(waitpid(child, &status, 0), _ret == child);
		errno = open_errno;
		__tests_failed++;
		fprintf(stderr,
			"%s: `open(fifo_path, O_WRONLY | O_CLOEXEC | O_NONBLOCK)` failed [got %s]\n",
			__func__, strerror(errno));
		TEST_SUCC(close(fifo_fd));
		TEST_SUCC(unlink(fifo_path));
		return;
	}
	TEST_RES(write(writer_fd, "X", 1), _ret == 1);
	TEST_SUCC(close(writer_fd));

	TEST_RES(waitpid(child, &status, 0),
		 _ret == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);

	TEST_SUCC(close(fifo_fd));
	TEST_SUCC(unlink(fifo_path));
}
END_TEST()
