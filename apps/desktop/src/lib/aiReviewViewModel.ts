import type { FindingDecisionStatus, ReviewFindingsValidationState } from "./commands";

export type ReviewOutcomeState = "complete" | "partial" | "invalid" | "empty";

export function reviewOutcomeState(
  validationState: ReviewFindingsValidationState,
  findingCount: number,
): ReviewOutcomeState {
  if (validationState === "invalid") return "invalid";
  if (findingCount === 0) return "empty";
  return validationState;
}

export function reviewOutcomeLabel(state: ReviewOutcomeState): string {
  switch (state) {
    case "complete": return "검토 완료";
    case "partial": return "부분 검토";
    case "invalid": return "근거 검증 실패";
    case "empty": return "유효한 finding 없음";
  }
}

export function findingDecisionLabel(status: FindingDecisionStatus): string {
  switch (status) {
    case "approved": return "승인됨";
    case "rejected": return "거절됨";
    case "pending": return "미결정";
  }
}
