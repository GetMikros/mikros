/* Prints what the libraries of the project say: the shared one with a
   version, the one without, and the static one. */
#include <stdio.h>

const char *tally_name(void);
int tally_base(void);
int tally_count(void);

int main(void)
{
	printf("%s, %d\n", tally_name(), tally_base() + tally_count());
	return 0;
}
