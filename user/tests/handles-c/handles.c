/* A C program for the C library tests of channel and handle
 * descriptors and of the functions that turn handles into descriptors
 * and back. It exits with 0 when every check passed, and otherwise with
 * the number of the first check that failed. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <mikros/startup.h>
#include <poll.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <unistd.h>

/* The descriptor kinds, and the kernel's status of success. */
enum { PIPE = 2, MEMORY = 4, CHANNEL = 9, HANDLE = 10 };

static int ready(int fd, short events)
{
	struct pollfd p = {.fd = fd, .events = events};
	return poll(&p, 1, 0) == 1 ? p.revents : 0;
}

int main(void)
{
	/* A channel descriptor is writable, readable once a message waits,
	 * and is neither read nor written itself. */
	mikros_handle_t ours, theirs;
	if (mikros_channel_create(0, &ours, &theirs) != 0)
		return 10;
	int channel = mikros_fd_from_handle(ours, CHANNEL);
	if (channel < 0)
		return 11;
	if (ready(channel, POLLIN | POLLOUT) != POLLOUT)
		return 12;
	if (mikros_channel_write(theirs, (const uint8_t *)"x", 1, NULL, 0) != 0)
		return 13;
	if (!(ready(channel, POLLIN) & POLLIN))
		return 14;
	char c;
	if (read(channel, &c, 1) != -1 || errno != EINVAL)
		return 15;
	if (write(channel, "x", 1) != -1 || errno != EINVAL)
		return 16;
	int epoll = epoll_create1(0);
	struct epoll_event e = {.events = EPOLLIN};
	struct epoll_event got;
	if (epoll < 0 || epoll_ctl(epoll, EPOLL_CTL_ADD, channel, &e) ||
	    epoll_wait(epoll, &got, 1, 0) != 1)
		return 17;
	mikros_handle_close(theirs);
	if (!(ready(channel, POLLIN) & (POLLIN | POLLHUP)))
		return 18;

	/* A handle descriptor holds any object and is never ready. */
	mikros_handle_t event;
	if (mikros_event_create(0, &event) != 0)
		return 20;
	int held = mikros_fd_from_handle(event, HANDLE);
	if (held < 0 || ready(held, POLLIN | POLLOUT) != 0)
		return 21;
	if (read(held, &c, 1) != -1 || errno != EINVAL)
		return 22;
	if (mmap(NULL, 4096, PROT_READ, MAP_SHARED, held, 0) != MAP_FAILED || errno != EINVAL)
		return 23;

	/* An object of another type than the kind's, and no handle at all,
	 * are refused, and the handle stays the caller's. */
	mikros_handle_t other;
	if (mikros_event_create(0, &other) != 0)
		return 30;
	if (mikros_fd_from_handle(other, MEMORY) != -1 || errno != EINVAL)
		return 31;
	if (mikros_fd_from_handle(other, 6) != -1 || errno != EINVAL)
		return 32;
	if (mikros_handle_close(other) != 0)
		return 33;
	if (mikros_fd_from_handle(other, HANDLE) != -1 || errno != EBADF)
		return 34;

	/* A memory descriptor's handle, made a descriptor again, maps the
	 * same object. */
	int memory = memfd_create("handles-c", 0);
	if (memory < 0 || write(memory, "hi", 2) != 2)
		return 40;
	mikros_handle_t object;
	if (mikros_handle_from_fd(memory, &object) != 0)
		return 41;
	int again = mikros_fd_from_handle(object, MEMORY);
	char *mapped = mmap(NULL, 4096, PROT_READ, MAP_SHARED, again, 0);
	if (again < 0 || mapped == MAP_FAILED || memcmp(mapped, "hi", 2) != 0)
		return 42;

	/* Pipes, channels and handles give their handles; files do not, nor
	 * does a descriptor that is not open. */
	int fds[2];
	mikros_handle_t h;
	if (pipe(fds) || mikros_handle_from_fd(fds[0], &h) != 0 || mikros_handle_close(h))
		return 50;
	if (mikros_handle_from_fd(channel, &h) != 0 || mikros_handle_close(h))
		return 51;
	if (mikros_handle_from_fd(held, &h) != 0 || mikros_handle_close(h))
		return 52;
	int file = open("/system/bin", O_RDONLY | O_DIRECTORY);
	if (file < 0 || mikros_handle_from_fd(file, &h) != -1 || errno != EINVAL)
		return 53;
	if (mikros_handle_from_fd(900, &h) != -1 || errno != EBADF)
		return 54;
	return 0;
}
