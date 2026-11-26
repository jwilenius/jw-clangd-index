import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { SerialQueue } from "../src/serialQueue";

const FAILING_ITEM = "b";

describe("SerialQueue", () => {
  it("runs one item at a time, in order, across several adds", async () => {
    const events: string[] = [];
    const queue = new SerialQueue<string>(
      async (item) => {
        events.push(`start ${item}`);
        await new Promise((resolve) => setImmediate(resolve));
        events.push(`end ${item}`);
      },
      () => assert.fail("no item throws"),
    );

    queue.add(["a", "b"]);
    queue.add(["c"]);
    await queue.whenIdle();

    assert.deepEqual(events, ["start a", "end a", "start b", "end b", "start c", "end c"]);
  });

  it("reports an item that throws and still runs the rest", async () => {
    const ranItems: string[] = [];
    const failures: string[] = [];
    const queue = new SerialQueue<string>(
      async (item) => {
        ranItems.push(item);
        if (item === FAILING_ITEM) {
          throw new Error(`cannot relocate ${item}`);
        }
      },
      (item, error) => failures.push(`${item}: ${String(error)}`),
    );

    queue.add(["a", FAILING_ITEM, "c"]);
    await queue.whenIdle();

    assert.deepEqual(ranItems, ["a", "b", "c"]);
    assert.deepEqual(failures, ["b: Error: cannot relocate b"]);
  });
});
