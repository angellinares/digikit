#ifndef SHOT_PARAMS_H
#define SHOT_PARAMS_H
// SHOT parameter words, shared by src/module.c and src/dsp/shot.s.
// The DSP reads the words in this order (R12 points at them).
#define SHOT_P_SMP 0
#define SHOT_P_START 1
#define SHOT_P_END 2
#define SHOT_P_PLAY 3
#define SHOT_P_PITCH 4
#define SHOT_P_COUNT 5
#endif
