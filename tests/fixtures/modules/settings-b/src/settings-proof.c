/* test/settings-b: the code of test/settings-a with its own command numbers (0x3a, 0x3b), run from a job `done` callback (the other way to reach the UI task). */
#define PROOF_CMD 0x3a
#define PROOF_VIA_JOB
#include "../../settings-a/src/settings-proof.c"
