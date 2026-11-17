# Crashing module (test)

The one fault injector of the fixtures. The store file `cfg/crash-after.bin` sets the delay (DSP blocks after its first tick), the mode (0 illegal instruction, 1 division by zero, 2 reset of the machine, 3 hang of the UI task, 4 reset in `init`) and, optionally, the place (0 `tick`, 2 the frame callback of the boot animation). No file: it never faults, so a test must write the file and assert that the fault happened.
It tests the digikit runtime's crash capture (docs/findings/46-digikit-debug.md): the firmware's exception handler calls the runtime's hook, which records the
fault in RAM that a soft reset keeps and resets; the next boot reports the record. The resets and hangs test safe mode (docs/findings/37-boot-load.md) and the intro callback (`modules/tools/bootscreen`). Test fixture only.
