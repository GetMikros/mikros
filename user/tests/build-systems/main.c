/* Prints what the shared and the static library of the project say. */
#include <stdio.h>

const char *tally_name(void);
int tally_count(void);

int main(void)
{
	printf("%s, %d\n", tally_name(), tally_count());
	return 0;
}
