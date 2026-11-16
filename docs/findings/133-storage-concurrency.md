# Storage concurrency on DT2 and DN2

Can several tasks write to the internal eMMC at the same time, and would it be faster? No: the controller and the firmware serialise every card transfer. Static reading of DT2 OS 1.17, DN2 OS 1.12 and the MCF5441x Reference Manual (out/refs/MCF5441XRM). **[D]** until a second check against the image bytes.

## Controller

- The eSDHC has one command channel and one data channel with one state machine each (RM p0619). No command queue, no second slot, no packed-command engine; SD Host 2.0 and MMC 4.2 only (p0619).
- It can send a command without data (CMD13) during a data transfer (p0620); errata SECF214 says such a command can see a spurious CRC or index error.
- Bus width 1 or 4 bit; the 8-bit pins are not bonded (p0292). No DDR mode (MMC 4.2).
- Card clock = base / (2·SDCLKFS · (DVS+1)) (SYSCTL, p0637). The RM gives 40 MHz as the eSDHC maximum (p0199, p0620). The firmware sets SDCLKFS=1, DVS=0 (DT2 setup 0x4012d78e, around 0x4012da42): 75.43 / 2 = 37.7 MHz (finding 126).
- FIFO 128 × 32 bit; the controller stops the card clock on FIFO underrun or overrun (p0657, p0665). The firmware's eDMA request size is 16 bytes (finding 126); the watermark it programs is not checked **[O]**.
- Wire ceiling at 37.7 MHz × 4 bit: about 18.5 MB/s payload; write plus full readback about 9.3 MB/s. Measured: write 15.3, read 16.1, write plus readback 7.8 MB/s (finding 126).

## Firmware driver

- Block read and write: DT2 0x4012e1b6 / 0x4012e39c, DN2 0x4012c87a / 0x4012ca60.
- Each transfer runs under one global mutex (DT2 0x47de3cac), plus a second mutex 0x44e4ae98 taken through 0x401344dc. Requests above 64 KiB are split into 64 KiB chunks, each locked separately.
- The caller starts eDMA channel 59 and the eSDHC, then waits with `sem_pend` on 0x44e3fec8 (command complete) and 0x44e3fec0 / 0x44e3feb8 (data). The eSDHC ISR 0x4012d5ba (vector 223) posts 0x44e3fec8 on CC/CTOE and 0x44e3fec0 on TC or a data error. The poster of 0x44e3feb8 is not found **[O]**. So the task sleeps during DMA and card busy: other tasks get the CPU.
- Writes: CMD23 with bit 31 (reliable write) before every CMD25, no auto-CMD12. Reads: CMD18 with auto-CMD12, no CMD23.
- Setup: CMD6 HS_TIMING=1, BUS_WIDTH=1 (4 bit), ERASE_GROUP_DEF=1; CMD16 512. No cache enable or flush, packed commands or command queue in the setup code seen.

## RTOS (DT2 1.17; the DN2 1.12 mutex functions are at the same addresses)

- Priority-preemptive, larger number = higher priority; context switch by trap #0 (0x40000410). No tick that rotates tasks of equal priority was found **[O]**.
- Binary semaphores: `sem_init` 0x40001398, `sem_pend` 0x4000141a, `sem_post` 0x4000155c. A counting variant exists (0x4000148c, 0x400014fc, 0x4000172a), not anchored.
- Mutexes `mutex_lock` 0x400015a0, `mutex_unlock` 0x400016d2: recursive for the owner, FIFO waiters, no priority inheritance.
- No task delete, no yield, no event flags found.

## +Drive file system (DT2)

- One global recursive mutex 0x46f4deb0 around every `file.*` and `path.*` call (finding 120). The close (`FUN_4015b3aa` → `FUN_4015b1e8`) holds it for the whole readback and hash-table work.
- Several files can be open (open bitmap 0x46f4ded0), but every call serialises on the mutex and they share static scratch buffers (0x4066d8c0, 0x406658c0, 0x44f2bc68).

## Consequences

- Several writers (tasks, files or pieces of one file) cannot raise card throughput: they take turns on the same mutexes and the same bus.
- A pipeline is safe and can help: one task waits on the card while another receives, copies or hashes in RAM. The task that holds the card mutex must not be delayed by middle-priority work (no priority inheritance).
- The gap between today's uploads and the card ceiling is serial stages and per-file cost. Plan: [usb-fast-batch](../plans/usb-fast-batch.md).
