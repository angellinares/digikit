/* Private logical-message consumer; no transport, allocator or public ABI. */
typedef unsigned int u32;
/* Outcomes: IDLE=0, PENDING=1, DRAINED=2, FAILED=3 (quarantined). */
volatile u32 close_command[4];
volatile u32 close_outcome;
volatile u32 close_last_id;

u32 close_begin(u32 id, u32 track, u32 epoch) {
    if ((close_outcome != 0 && close_outcome != 2) ||
        id == 0 || id > 0x7fffffff || id <= close_last_id ||
        track > 1 || epoch == 0 || epoch > 0x7fffffff)
        return 0;
    close_command[0] = 0x01014243;
    close_command[1] = id;
    close_command[2] = track;
    close_command[3] = epoch;
    close_last_id = id;
    close_outcome = 1;
    return 1;
}

/* Caller supplies a complete immutable snapshot, never concurrently written. */
u32 close_observe(const u32 ack[4]) {
    if (close_outcome != 1)
        return close_outcome;
    if (ack[1] != close_command[1] || ack[2] != close_command[2] ||
        ack[3] != close_command[3])
        return close_outcome;
    if ((ack[0] & 0x00ffffff) != 0x00014241)
        close_outcome = 3;
    else {
        u32 status = ack[0] >> 24;
        if (status == 2)
            close_outcome = 2;
        else if (status != 1)
            close_outcome = 3;
    }
    return close_outcome;
}

u32 close_timeout(void) {
    if (close_outcome == 1)
        close_outcome = 3;
    return close_outcome;
}

u32 close_test_observe(u32 header, u32 id, u32 track, u32 epoch) {
    const u32 snapshot[4] = {header, id, track, epoch};
    return close_observe(snapshot);
}
