/** Runs items one at a time, in the order they were added; an item whose run throws is reported and the next one still runs. */
export class SerialQueue<T> {
  private tail: Promise<void> = Promise.resolve();

  constructor(
    private readonly run: (item: T) => Promise<void>,
    private readonly onError: (item: T, error: unknown) => void,
  ) {}

  add(items: readonly T[]): void {
    for (const item of items) {
      this.tail = this.tail.then(() => this.run(item)).catch((error: unknown) => this.onError(item, error));
    }
  }

  /** Resolves once every item added so far has run. */
  whenIdle(): Promise<void> {
    return this.tail;
  }
}
