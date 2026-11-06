# Repository guidance

This repository is a local-first desktop tool for visual code intelligence and
agent-assisted refactoring. `SPEC.md` is the product and architecture source of
truth; read the relevant sections before making design or scope decisions.

## Product constraints

- The MVP is a Tauri 2 desktop app with a bundled SolidJS/TypeScript UI and a
  Rust core. Keep runtime functionality local; do not add an HTTP backend,
  external database, JVM, or container requirement.
- The first analysis target is Kotlin / Spring Boot. The app analyzes target
  projects; it does not build or run them.
- Use the system Git CLI for Git features. File browsing must still work when
  Git is unavailable.
- Keep the MVP graph viewer in SolidJS and SVG. Tree-sitter belongs to Phase 2;
  language servers and advanced graph dependencies are later work.
- Treat analyzers as the source of verifiable facts and AI as an interpreter
  of evidence. Findings must cite source locations.

## Safety and user control

- Keep analysis local by default. Do not transfer project content to an
  external service without an explicit user action and approval.
- Never make automated edits in the user's original working tree. Use an
  isolated worktree for automated fixes and preserve pre-existing changes.
- Present and inspect diffs before integration. Get approval for each fix;
  commits and remote Git actions require their own approval.
- Run relevant checks before integration. Report skipped or unavailable checks
  as such; never describe them as passing.
- Keep fixes independently reviewable and verifiable where practical.

## Implementation practices

- Follow existing patterns and keep MVP modules small. Avoid introducing a
  framework, service, dependency, or abstraction unless the task requires it.
- Keep UI concerns in the SolidJS/TypeScript frontend and filesystem, indexing,
  persistence, Git, and policy operations in the Rust/Tauri core.
- Make analyzers and AI providers extensible through adapters, without adding
  future language targets to the MVP by default.
- Update `SPEC.md` when a requested product or architecture decision changes
  its stated requirements; keep this file focused on durable agent guidance.
- Do not invent build or test commands. Use commands documented by the repo or
  configured in its manifests, and report when no relevant command exists.

## Issues, branches, commits, and pull requests

- Write issues and pull requests in Korean by default. Keep product and code
  identifiers in their original form when useful.
- When creating or updating an issue, follow the matching template under
  `.github/ISSUE_TEMPLATE/`. When opening a pull request, follow
  `.github/PULL_REQUEST_TEMPLATE.md`; do not omit sections without marking
  them not applicable.
- Create a branch for each issue and avoid working directly on the default
  branch. Use `<type>/<issue-number>-<short-kebab-case-description>`, for
  example `feat/12-project-scanner` or `fix/27-symlink-boundary`.
- Keep an issue focused on one user outcome or technical result. Split
  independent work into separate issues. A pull request should normally
  resolve one issue; explain when inseparable changes must be combined.
- Keep each commit to one coherent, reviewable change. Use
  `<type>(<scope>): <한국어 변경 요약>` (for example,
  `feat(scanner): Gradle 프로젝트 감지 추가`). Common types are `feat`,
  `fix`, `refactor`, `docs`, `test`, and `chore`. Put `Refs #123` or
  `Closes #123` in the commit body when linking an issue.
- Use exactly one scope from the catalog below. Choose the narrowest scope that
  owns the change; use `global` only when it affects multiple major areas or
  changes a repository-wide rule/architecture. Do not invent a scope casually;
  propose and add a new one here when a durable product area is missing.

  | Scope | Use for |
  | --- | --- |
  | `global` | Cross-area changes, shared architecture, or repository-wide behavior/policy |
  | `app` | Tauri app shell, startup/shutdown, window lifecycle, and app-wide orchestration |
  | `ui` | Shared UI shell, reusable components, styling, and frontend-wide interaction patterns |
  | `project` | Project registration, opening, configuration, and project metadata |
  | `scanner` | File discovery/filtering, stack detection, file index, and Git status scanning |
  | `analyzer` | Language parsing, symbols, references, calls, and generic analyzer interfaces |
  | `graph` | Knowledge graph model/queries, graph snapshots, visualization, and navigation |
  | `api` | Framework endpoint extraction and API route/details features |
  | `review` | Review sessions, findings, evidence display, and approve/reject lifecycle |
  | `agent` | AI context building, review/patch agent runtime, and provider adapters |
  | `patch` | Patch planning/application, isolated fix workspaces, and diff review |
  | `verification` | Command policy, build/lint/test execution, and verification results |
  | `git` | Worktrees, branches, commits, integration, push, and pull requests |
  | `persistence` | SQLite schema, migrations, storage, and task recovery |
  | `security` | Access boundaries, secret handling, external transfer, and execution safety |
  | `docs` | Product/technical documentation and repository guidance |
  | `tooling` | Build configuration, development tooling, packaging, and CI |

- A pull request description must summarize the change, link the issue, state
  verification results, and identify checks that were skipped or unavailable.
  Include screenshots for relevant UI changes and note risks or follow-up work.
- If a pull request has no linked issue, use squash merge. Otherwise follow the
  repository setting or the user's merge instruction.
- Do not create issues, branches, commits, pull requests, or perform remote Git
  actions unless the user has authorized that action. Follow the existing
  approval requirements for commits and remote operations.

### Issue label categories

GitHub does not provide native label groups, so keep the repository's label
names in English and use this color-and-purpose convention. Label descriptions
may be in Korean.

| Category | Labels | Color convention |
| --- | --- | --- |
| Work type | `bug` (red `#D73A4A`), `feature` (green `#1F883D`), `fix` (amber `#FBCA04`), `refactor` (purple `#8250DF`), `task` (slate `#59636E`), `documentation` (blue `#0969DA`) | Apply one. `bug` is a defect report; `fix` is the implementation work to resolve a confirmed defect. |
| Priority | `priority: critical` (dark red `#B60205`), `priority: high` (orange `#D93F0B`), `priority: medium` (yellow `#FBCA04`), `priority: low` (green `#1F883D`) | Apply one after triage. Critical means security/data-loss or a release-blocking failure; high blocks a core MVP flow; medium is normal planned work; low is polish or optional improvement. |
| Status | `status: blocked` (gray `#6E7781`) | Add only while progress depends on an external decision or prerequisite; remove when unblocked. |
| Contribution | `help wanted` (teal `#008672`), `good first issue` (violet `#7057FF`) | Add when inviting or guiding contributors. |
| Triage | `duplicate` (gray `#CFD3D7`), `wontfix` (dark gray `#6E7781`) | Apply as needed during issue triage. |
| Accessibility | `accessibility` (pink `#F143AB`) | Add alongside a work-type label when relevant. |

Keep these categories, colors, and the issue-template labels synchronized with
the remote repository. Do not create near-duplicate labels when an existing
category already describes the issue. Do not use generic `question` or
`invalid` labels; answer questions directly or close out-of-scope reports with
an explanation.

### Roadmap milestones

Use GitHub Milestones as the roadmap tracker, mapped to the delivery phases in
`SPEC.md`. Keep these exact milestone titles and use Korean descriptions with
the phase deliverables and acceptance criteria:

| Milestone | Scope |
| --- | --- |
| `Phase 1 — 프로젝트 탐색` | Tauri app, project scan/detection, Git status, and basic SQLite index |
| `Phase 2 — 코드 인텔리전스` | Kotlin/Java analysis, Spring analysis, knowledge graph, and diagrams |
| `Phase 3 — AI 코드 리뷰` | Scoped context, provider adapters, evidence-backed findings, and per-finding decisions |
| `Phase 4 — 자동 패치` | Isolated patching, diff review, verification, and bounded retries |
| `Phase 5 — Git 워크플로` | Per-fix worktrees/branches, commits, integration, conflict handling, and approved remote actions |
| `Phase 6 — 확장` | Additional languages/frameworks, incremental analysis, optimization, local models, and plugins |

- The user authorizes agents to maintain these six repository milestones as a
  standing part of authorized issue work: create any missing milestone, keep
  descriptions aligned with `SPEC.md`, assign issues to the most relevant
  phase, and close a milestone only after its SPEC acceptance criteria are met
  and all issues assigned to it are closed.
- When creating or materially updating an issue, assign exactly one milestone
  based on the phase that delivers the issue's outcome. Split independent
  cross-phase work into separate issues. If the phase is genuinely unclear,
  leave the milestone unset and explain the ambiguity rather than guessing.
- Re-evaluate an issue's milestone when its scope changes. Do not assign dates
  unless the user provides a schedule. Do not delete or rename milestones, or
  create additional roadmap milestones, without explicit user direction.
- Before milestone changes, inspect the remote milestone list and reuse exact
  existing titles; never create duplicates. Milestone progress is derived from
  its issues, so keep issue assignment and closure accurate.

## Working checklist

1. Read the relevant specification section and inspect nearby code before
   changing behavior.
2. Keep the change within the requested phase and preserve local-first,
   evidence-based, isolated, and human-approved workflows.
3. Add or update focused checks when the repository has a suitable test setup.
4. Summarize changed files, behavior, checks run, and any checks that could not
   be run.
