/* The shared library: which build system built it. */
const char *tally_name(void)
{
	return "built by " BUILD_SYSTEM;
}
