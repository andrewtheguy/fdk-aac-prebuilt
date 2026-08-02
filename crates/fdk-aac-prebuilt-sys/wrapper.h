/* The two headers that are fdk-aac's public interface, spelled the way the installed
 * layout spells them: cmake puts all six public headers in <prefix>/include/fdk-aac/, so
 * `-I include` and an `fdk-aac/` prefix is what a consumer compiling their own C against
 * DEP_FDK_AAC_INCLUDE would write.
 *
 * The other four (machine_type.h, FDK_audio.h, genericStds.h, syslib_channelMapDescr.h)
 * arrive through these two; they are quoted, same-directory includes, so one -I resolves
 * the whole set. */
#include <fdk-aac/aacenc_lib.h>
#include <fdk-aac/aacdecoder_lib.h>
