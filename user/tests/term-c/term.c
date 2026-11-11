/* A program that the terminal probe starts on a terminal, as its
 * standard input, output and error. It checks the terminal through the C
 * library's terminal functions, says the terminal's name, reads the lines
 * the probe types, and exits with 0 when every check passed, and
 * otherwise with the number of the first check that failed. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>

static int say(const char *s)
{
	size_t n = strlen(s);
	return write(1, s, n) == (ssize_t)n;
}

int main(void)
{
	char buf[64];
	struct termios t;
	struct winsize ws;
	struct stat st;
	if (!isatty(0) || !isatty(1) || !isatty(2))
		return 10;
	if (tcgetattr(0, &t) || !(t.c_lflag & ICANON) || !(t.c_lflag & ECHO))
		return 11;
	if (ioctl(0, TIOCGWINSZ, &ws) || ws.ws_row != 24 || ws.ws_col != 80)
		return 12;
	if (fstat(0, &st) || !S_ISCHR(st.st_mode))
		return 13;
	/* The terminal's name, from its number, as both functions give it. */
	char *name = ttyname(0), pts[32];
	if (!name || ptsname_r(0, pts, sizeof pts) || strcmp(name, pts) != 0)
		return 22;
	if (strncmp(name, "/dev/pts/", 9) != 0)
		return 23;
	if (!say("ready ") || !say(name) || !say("\n"))
		return 14;
	/* A canonical read returns the line the probe typed. */
	ssize_t n = read(0, buf, sizeof buf);
	if (n != 5 || memcmp(buf, "line\n", 5) != 0)
		return 15;
	t.c_lflag &= ~(ICANON | ECHO);
	t.c_cc[VMIN] = 1;
	t.c_cc[VTIME] = 0;
	if (tcsetattr(0, TCSANOW, &t))
		return 16;
	if (!say("raw\n"))
		return 17;
	/* Without ICANON a single byte is readable, which poll sees. */
	struct pollfd p = { .fd = 0, .events = POLLIN };
	if (poll(&p, 1, 10000) != 1 || !(p.revents & POLLIN))
		return 18;
	if (read(0, buf, 1) != 1 || buf[0] != 'x')
		return 19;
	/* With O_NONBLOCK and nothing typed, a read fails at once. */
	if (fcntl(0, F_SETFL, O_NONBLOCK) || read(0, buf, 1) != -1 || errno != EAGAIN)
		return 20;
	if (!say("done\n"))
		return 21;
	return 0;
}
