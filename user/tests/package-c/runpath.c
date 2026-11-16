/* A library that a package keeps in lib/, which its program names in a
   DT_RUNPATH of $ORIGIN/../lib. */
const char *from_runpath(void)
{
	return "runpath";
}
