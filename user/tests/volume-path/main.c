/* A program of the USB volumes run, which the volumes probe starts with a
 * path below /volumes/<name>/ and the path that realpath must give for it,
 * below /volumes/<id>/. It exits with 0 when realpath gives that path, with
 * 1 when it gives another, and with 2 when it fails. */
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv)
{
	char resolved[PATH_MAX];
	if (argc != 3 || !realpath(argv[1], resolved))
		return 2;
	if (strcmp(resolved, argv[2]) != 0) {
		fprintf(stderr, "volume-path: %s is %s, not %s\n", argv[1], resolved, argv[2]);
		return 1;
	}
	return 0;
}
