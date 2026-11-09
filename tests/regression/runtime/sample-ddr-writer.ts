// Private diagnostic transaction owner. This is not a public sample API.
export type DdrSend = (token: number, seq: number, op: number, offset?: number, data?: Buffer) => Promise<void>;

export class DdrWriter {
  private tail: Promise<void> = Promise.resolve();
  private previousToken: number | undefined;

  private readonly send: DdrSend;

  constructor(send: DdrSend) {
    this.send = send;
  }

  load(data: Buffer, token: number, open = true): Promise<void> {
    // Retain rejection on the owner chain even when the caller catches its result.
    // No later selection may mutate backing after uncertain completion.
    const result = this.tail.then(async () => {
      if (data.length !== 960000 || !Number.isInteger(token) || token <= 0 || token > 0xffffffff)
        throw new Error('diagnostic bank requires 960000 bytes and a nonzero uint32 token');
      if (this.previousToken !== undefined) {
        if (this.previousToken === 0xffffffff || token !== this.previousToken + 1)
          throw new Error('diagnostic generation must be the exact non-wrapping successor');
        await this.send(this.previousToken, 377, 4);
      }
      await this.send(token, 0, 1);
      for (let seq = 1; seq <= 375; seq++) {
        const offset = (seq - 1) * 2560;
        await this.send(token, seq, 2, offset / 4, data.subarray(offset, offset + 2560));
      }
      await this.send(token, 376, 3, 240000);
      this.previousToken = token;
      if (open) await this.send(token, 378, 5);
    });
    this.tail = result;
    // The owner retains the rejection above; this only prevents an unhandled
    // rejection when callers enqueue several selections before awaiting them.
    void this.tail.catch(() => {});
    return result;
  }
}
