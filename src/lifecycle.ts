// Which backend batch this page is allowed to render, kept free of Tauri
// imports so it can be unit-tested on its own.
//
// The rule this file exists to enforce: a reload creates a new page, not a new
// batch. The batch that was running carries on in the backend, and its progress
// keeps arriving. A fresh page has no rows those events could belong to, so it
// must not apply them -- and once the user starts a batch of their own, the old
// batch's events must not reach it either.

/** The backend's own answer about the cleaning pipeline. */
export type ProcessingState = {
  cleaning: boolean;
  /**
   * The newest batch the backend has started, `0` before the first one. While
   * `cleaning` it is the batch that is running. Application-internal: a counter
   * that tells this process's batches apart and says nothing about any file.
   */
  batchId: number;
};

/** Before the backend has been asked, and after it says nothing is running. */
export const IDLE_PROCESSING: ProcessingState = { cleaning: false, batchId: 0 };

/**
 * What to assume when the backend cannot be reached. Busy, always: every caller
 * of this uses it to decide whether it is safe to start cleaning, install an
 * update or close, and "I don't know" is not a safe answer to any of those.
 */
export const UNKNOWN_PROCESSING: ProcessingState = { cleaning: true, batchId: 0 };

/**
 * This page's claim on one backend batch.
 *
 * `null` until the page starts a batch itself -- which is where a reloaded page
 * begins, and why it applies no progress at all.
 *
 * A batch's id is only known once the backend starts it, and the Clean call
 * does not answer until the batch is over. So the claim is staked with a
 * watermark instead: the newest batch id that existed at the moment the page
 * asked, taken while nothing was running. Any batch this page then starts has a
 * higher id, and no batch that already existed can.
 */
export type BatchAttachment = {
  /** The newest batch id that existed before this page asked for its own. */
  watermark: number;
  /** The batch, once its first event has named it. */
  id: number | null;
};

/** Staked before Clean is invoked. `state` must say nothing is running. */
export function attach(state: ProcessingState): BatchAttachment {
  return { watermark: state.batchId, id: null };
}

/**
 * Whether a progress event may be applied, and the claim to hold on to.
 *
 * The first event above the watermark is this page's batch and pins the claim;
 * after that only that exact batch is accepted. An event from a batch the page
 * never started -- one that outlived a reload, or one that finished before the
 * current batch began -- is refused, so it can never write into a row.
 */
export function applyProgress(
  attachment: BatchAttachment | null,
  batchId: number,
): { accepted: boolean; attachment: BatchAttachment | null } {
  if (!attachment) return { accepted: false, attachment };
  if (attachment.id !== null) {
    return { accepted: batchId === attachment.id, attachment };
  }
  if (batchId <= attachment.watermark) return { accepted: false, attachment };
  return { accepted: true, attachment: { ...attachment, id: batchId } };
}

/**
 * Whether a batch is running that this page is not showing: one it started and
 * then forgot across a reload, or one it was refused a turn behind. The queue
 * on screen has nothing to do with it, so the page says so and offers no Clean.
 */
export function cleaningElsewhere(
  state: ProcessingState,
  attachment: BatchAttachment | null,
): boolean {
  if (!state.cleaning) return false;
  if (!attachment) return true;
  if (attachment.id !== null) return state.batchId !== attachment.id;
  // Staked but not yet named by an event. Only this page starts batches, so a
  // batch newer than the watermark is the one it just asked for.
  return state.batchId <= attachment.watermark;
}

/** How often to ask the backend whether a batch it owns has finished. */
export const PROCESSING_POLL_MS = 1000;

export const CLEANING_ELSEWHERE_MESSAGE =
  "A cleaning batch started earlier is still running. Wait for it to finish before starting another.";

export const CLOSE_BLOCKED_MESSAGE =
  "Cleaning is still in progress. Wait for it to finish before closing MetaStrip.";
