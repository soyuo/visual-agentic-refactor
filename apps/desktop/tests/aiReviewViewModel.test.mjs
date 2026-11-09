import assert from "node:assert/strict";
import test from "node:test";

import {
  findingDecisionLabel,
  reviewOutcomeLabel,
  reviewOutcomeState,
} from "../src/lib/aiReviewViewModel.ts";

test("review outcome distinguishes complete, partial, invalid, and empty results", () => {
  assert.equal(reviewOutcomeState("complete", 2), "complete");
  assert.equal(reviewOutcomeState("partial", 1), "partial");
  assert.equal(reviewOutcomeState("invalid", 0), "invalid");
  assert.equal(reviewOutcomeState("complete", 0), "empty");
  assert.equal(reviewOutcomeLabel("partial"), "부분 검토");
  assert.equal(reviewOutcomeLabel("empty"), "유효한 finding 없음");
});

test("finding decision labels preserve each independent persisted state", () => {
  assert.equal(findingDecisionLabel("pending"), "미결정");
  assert.equal(findingDecisionLabel("approved"), "승인됨");
  assert.equal(findingDecisionLabel("rejected"), "거절됨");
});
