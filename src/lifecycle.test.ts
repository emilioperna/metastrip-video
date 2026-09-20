import { describe, expect, it } from "vitest";
import {
  CLEANING_ELSEWHERE_MESSAGE,
  CLOSE_BLOCKED_MESSAGE,
  IDLE_PROCESSING,
  UNKNOWN_PROCESSING,
  applyProgress,
  attach,
  cleaningElsewhere,
  type BatchAttachment,
  type ProcessingState,
} from "./lifecycle";

const idle = (batchId: number): ProcessingState => ({ cleaning: false, batchId });
const cleaning = (batchId: number): ProcessingState => ({ cleaning: true, batchId });

/**
 * A page's rows, driven exactly the way the window drives them: an event is
 * applied only when the claim accepts it. What these tests are really about is
 * what is *not* in `rows` afterwards.
 */
function page(attachment: BatchAttachment | null = null) {
  const rows: string[] = [];
  return {
    get attachment() {
      return attachment;
    },
    clean(state: ProcessingState) {
      attachment = attach(state);
    },
    progress(batchId: number, what: string) {
      const claim = applyProgress(attachment, batchId);
      attachment = claim.attachment;
      if (claim.accepted) rows.push(what);
    },
    rows,
  };
}

describe("a page that has just loaded", () => {
  it("applies nothing, because it has started no batch", () => {
    // The reload case: the batch that was running carries on in the backend and
    // its progress keeps arriving, but this page has no rows it could belong to.
    const fresh = page();
    fresh.progress(7, "file one processing");
    fresh.progress(7, "file one verified");

    expect(fresh.rows).toEqual([]);
  });

  it("stays detached however many batches go past", () => {
    const fresh = page();
    for (const batch of [1, 2, 3]) fresh.progress(batch, `batch ${batch}`);

    expect(fresh.rows).toEqual([]);
    expect(fresh.attachment).toBeNull();
  });
});

describe("claiming a batch", () => {
  it("takes the first batch newer than the one the backend reported", () => {
    // Nothing is running and batch 4 was the last one. Whatever this Clean
    // starts is batch 5 or later; nothing that already exists can be.
    const session = page();
    session.clean(idle(4));
    expect(session.attachment).toEqual({ watermark: 4, id: null });

    session.progress(5, "processing");
    expect(session.attachment).toEqual({ watermark: 4, id: 5 });
    expect(session.rows).toEqual(["processing"]);
  });

  it("keeps the same batch for every event of that batch", () => {
    const session = page();
    session.clean(idle(0));
    session.progress(1, "a processing");
    session.progress(1, "a verified");
    session.progress(1, "b processing");
    session.progress(1, "b verified");

    expect(session.attachment).toEqual({ watermark: 0, id: 1 });
    expect(session.rows).toHaveLength(4);
  });
});

describe("events that do not belong to this page", () => {
  it("refuses the batch that was running when the page reloaded", () => {
    // Batch 9 outlived the reload. The page waits for it, then cleans: its own
    // batch is 10, and 9's remaining events must not touch it.
    const session = page();
    session.clean(idle(9));
    session.progress(9, "batch 9 still going");
    session.progress(10, "our first file");
    session.progress(9, "batch 9 finishing");

    expect(session.rows).toEqual(["our first file"]);
  });

  it("refuses a late event from the batch before this one", () => {
    // Batch A ended, batch B started, and an event from A arrives after.
    const session = page();
    session.clean(idle(0));
    session.progress(1, "A processing");
    session.clean(idle(1));
    session.progress(2, "B processing");
    session.progress(1, "A, late");

    expect(session.rows).toEqual(["A processing", "B processing"]);
  });

  it("refuses a batch older than the claim even before the claim is named", () => {
    const session = page();
    session.clean(idle(3));
    session.progress(3, "the batch before ours");
    session.progress(1, "an older one still");

    expect(session.rows).toEqual([]);
    expect(session.attachment).toEqual({ watermark: 3, id: null });
  });

  it("refuses a different batch once the claim is named", () => {
    const session = page();
    session.clean(idle(0));
    session.progress(1, "ours");
    session.progress(2, "not ours");

    expect(session.rows).toEqual(["ours"]);
  });
});

describe("a batch this page is not showing", () => {
  it("is what a page sees when it reloads mid-batch", () => {
    expect(cleaningElsewhere(cleaning(4), null)).toBe(true);
  });

  it("is not this page's own batch", () => {
    const session = page();
    session.clean(idle(4));
    // Claimed, and the backend's next answer is the batch it just started --
    // before and after that batch has named itself in an event.
    expect(cleaningElsewhere(cleaning(5), session.attachment)).toBe(false);
    session.progress(5, "processing");
    expect(cleaningElsewhere(cleaning(5), session.attachment)).toBe(false);
  });

  it("is a batch other than the one this page claimed", () => {
    const session = page();
    session.clean(idle(4));
    session.progress(5, "processing");
    expect(cleaningElsewhere(cleaning(6), session.attachment)).toBe(true);
  });

  it("is nothing at all once the backend says it is not cleaning", () => {
    expect(cleaningElsewhere(idle(0), null)).toBe(false);
    expect(cleaningElsewhere(idle(9), null)).toBe(false);
  });
});

describe("what the page assumes", () => {
  it("starts from nothing running, so a launch shows no stale warning", () => {
    expect(IDLE_PROCESSING).toEqual({ cleaning: false, batchId: 0 });
  });

  it("treats a backend it cannot reach as busy", () => {
    // Every caller uses this to decide whether cleaning, installing an update
    // or closing is safe. "I don't know" is not a safe answer to any of them.
    expect(UNKNOWN_PROCESSING.cleaning).toBe(true);
    expect(cleaningElsewhere(UNKNOWN_PROCESSING, null)).toBe(true);
  });
});

describe("what the window says", () => {
  it("tells the user to wait rather than offering a second batch", () => {
    expect(CLEANING_ELSEWHERE_MESSAGE).toBe(
      "A cleaning batch started earlier is still running. Wait for it to finish before starting another.",
    );
  });

  it("explains a close that was refused", () => {
    expect(CLOSE_BLOCKED_MESSAGE).toBe(
      "Cleaning is still in progress. Wait for it to finish before closing MetaStrip.",
    );
  });
});
