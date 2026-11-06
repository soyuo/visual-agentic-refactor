# Visual Code Intelligence & Agentic Refactoring Platform

## System Specification

**Version:** 1.0  
**Type:** Product requirements and architecture  
**Platform:** Windows, macOS, Linux  
**MVP app stack:** Tauri 2, SolidJS, TypeScript, Rust  
**MVP analysis target:** Kotlin / Spring Boot projects  
**Future targets:** Java, TypeScript, JavaScript, Python, and additional
frameworks

## 1. Purpose and Principles

Build a local-first desktop harness that helps developers understand a codebase
visually, review selected code with AI, and apply verified fixes through
isolated Git workspaces.

Users can explore files, modules, APIs, symbols, dependencies, call
relationships, and statically inferred execution flows. Every review finding
must cite source locations. Users approve each fix before it changes code;
remote Git actions require separate approval.

Design principles:

- **Local-first:** Analyze source and store project data locally by default.
- **Static-first:** Analyzers report verifiable facts; AI interprets evidence.
- **Evidence-based:** Findings link to exact files and locations.
- **Human-controlled:** Users approve edits, commits, and external transfers.
- **Isolated:** Automated edits never run in the original working tree.
- **Atomic:** Keep fixes and commits separate when practical.
- **Verifiable:** Run available checks before integration; never treat skipped
  checks as passed.
- **Extensible:** Add languages, frameworks, and AI providers through adapters.

## 2. User Workflow

1. Select a local project and scan its files, stack, dependencies, and Git
   state.
2. Analyze source structure and build a versioned Code Knowledge Graph.
3. Explore architecture, API routes, dependencies, and execution flow.
4. Select files or graph nodes for an AI review.
5. Inspect evidence-backed findings and approve or reject each one.
6. Create an isolated fix task for each approved finding.
7. Generate a patch, inspect its diff, and run project-appropriate checks.
8. Approve commits and integrate validated changes.
9. Optionally push or create a pull request after explicit approval.

For multiple findings, create independent worktrees and validate each fix
separately. Integrate in order, check for conflicts, then run final
verification.

## 3. MVP Runtime and Architecture

### 3.1 Runtime constraints

The MVP is one Tauri desktop app containing a bundled SolidJS UI and Rust core.

- Build the UI into static assets; no Node.js development server is needed at
  runtime.
- Rust handles file discovery, indexing, Git CLI calls, SQLite, and Tauri
  commands.
- Do not require an HTTP backend, JVM, Kotlin compiler, container, or external
  database.
- Kotlin / Spring Boot is an analysis target. The MVP does not build or run the
  target project.
- Git features use the system Git CLI. File browsing still works if Git is
  unavailable.
- Do not install or launch language servers in the MVP. Add Tree-sitter symbol
  parsing in Phase 2 and LSP support later.
- Use SolidJS and SVG for the MVP graph viewer; defer a graph UI dependency and
  advanced layout.

### 3.2 Components

- **Desktop UI:** Project explorer, diagram viewer, details, review findings,
  diff and commit controls.
- **Tauri / Rust core:** Project scanner, command handlers, orchestration,
  persistence, Git operations, and execution policy.
- **Code intelligence:** Parsers, symbol index, framework analyzers,
  relationship resolver, and graph model.
- **Agent runtime:** Context builder, review and patch agents, provider
  adapters.
- **Verification engine:** Policy checks, build/test/lint execution, and diff
  validation.
- **SQLite:** Project snapshots, graph, jobs, findings, fixes, verification, and
  Git transaction metadata.

The proposed Rust crate layout is in Section 21. Keep MVP modules small; split
crates as the implementation grows.

## 4. Technology

| Area | MVP / target |
| --- | --- |
| Desktop | Tauri 2 |
| UI | SolidJS + TypeScript |
| Graph | SolidJS + SVG; Mermaid export |
| Native core | Rust |
| Parsing | Tree-sitter (Phase 2) |
| Semantic analysis | LSP (later phase) |
| Local storage | SQLite |
| AI | Multi-provider adapter |
| Version control | Git CLI |
| Initial analyzer | Kotlin / Spring Boot, static analysis |

SolidJS provides fine-grained UI updates without a separate state framework in
the MVP. Tauri packages the UI and local core in one desktop app. SVG keeps the
first graph view lightweight. SQLite stores local state. Git CLI provides
worktrees, diffs, commits, and optional remote operations.

## 5. MVP Scope and Delivery Phases

### Phase 1 — Project Explorer

Deliver:

- Tauri 2 + Rust app with bundled SolidJS / TypeScript UI.
- Directory selection, file scan, and file tree.
- Kotlin / Spring / Gradle detection from project files.
- Git status when Git is installed.
- SQLite persistence for project path and basic index.
- No backend service, JVM, or LSP process.

Acceptance: Selecting a directory displays its file structure and detected
project metadata.

### Phase 2 — Code Intelligence

Add Tree-sitter, Kotlin / Java symbol extraction, optional LSP, class and
function relations, Spring API analysis, graph snapshots, and architecture, API,
dependency, and flow diagrams.

Acceptance: Users can navigate major Kotlin / Spring APIs, classes, functions,
and their known relationships.

### Phase 3 — AI Review

Add scoped context building, provider adapters, structured findings with code
evidence, review UI, and per-finding approval or rejection.

### Phase 4 — Automated Patching

Add patch planning, isolated edits, diffs, project checks, failure reporting,
and retry limits.

### Phase 5 — Git Workflow

Add per-task worktrees and branches, independent commits, integration, conflict
handling, final checks, push, and pull request support.

### Phase 6 — Extensions

Add JavaScript / TypeScript and Python analyzers, more frameworks, incremental
analysis improvements, large-graph optimization, local LLM improvements, GitHub
integration, and analyzer plugins.

## 6. Project Scanner

Input: project path, ignore patterns, scan depth, and analysis options.

Processing: validate path; discover files; apply ignore, size, type, and binary
filters; detect languages and frameworks; inspect dependencies and Git; generate
the file index; pass supported source files to analyzers.

Initial Kotlin / Spring inputs:

- settings.gradle.kts and build.gradle.kts
- application.yml and application.properties
- Kotlin and Java source, including tests
- Gradle modules and Git metadata

Default exclusions: .git/objects, node_modules, .gradle, build, dist, target,
.venv, and binary artifacts. Allow project-level overrides.

Git metadata includes repository presence, branch, HEAD, tracked, modified and
untracked files, worktrees, and remotes. Read-only analysis is allowed with
uncommitted changes. Automated edits use a separate worktree and must preserve
those changes.

## 7. Code Analyzer

The analyzer combines syntax parsing, optional semantic data, and
framework-specific rules.

Tree-sitter targets: classes, functions, constructors, interfaces, inheritance,
imports, annotations, calls, conditionals, loops, and exception handlers.
Resolve uncertain calls with LSP when available.

LSP capabilities may include definition, references, document symbols, type
information, and implementations. Unsupported capabilities must be marked
unavailable, not inferred as confirmed.

Initial Spring MVC targets: RestController, Controller, RequestMapping,
GetMapping, PostMapping, PutMapping, PatchMapping, DeleteMapping, Service,
Repository, Component, constructor injection, and Transactional.

Static analysis cannot fully resolve dynamic bean registration, conditional
beans, AOP proxies, reflection, runtime route changes, multiple interface
implementations, or environment-dependent behavior. Mark such relationships as
inferred or unknown.

## 8. Code Knowledge Graph

The graph is the central model for code structure and relationships.

Node types: Project, Module, Directory, File, Package, Class, Interface,
Function, Method, API Endpoint, DTO, Dependency, External Service.

Edge types: CONTAINS, DECLARES, IMPORTS, DEPENDS_ON, CALLS, IMPLEMENTS, EXTENDS,
INJECTS, EXPOSES, USES_DTO, ACCESSES, REFERENCES.

Each node stores: ID, project and snapshot IDs, type, name, qualified name, file
path, source range, language, and metadata.

Each edge stores: ID, project and snapshot IDs, source and target node IDs,
relation type, confidence, and evidence.

Confidence values:

- **Confirmed:** Directly established by source and analyzer.
- **Inferred:** Derived from available evidence.
- **Unknown:** Not statically resolvable.

Never present an LLM inference as an analyzer-confirmed fact. Each snapshot
records HEAD, source file hashes, analysis time, analyzer version, and project
settings. Recompute only affected nodes and edges after changes.

## 9. Diagrams and UI

Diagram modes: project structure, architecture, API routes, dependencies, and
execution flow. Execution flow distinguishes inter-function calls from
intra-function control flow.

MVP graph interactions: zoom, pan, node selection, and source navigation. Later
add multi-select, connected-node highlighting, symbol search, filters, detail
inspection, and Mermaid export.

The detail panel shows node name/type, path and source range, related
classes/functions/APIs, calls and references, confidence, and a review action.

The review panel shows title, description, severity, impact, evidence, files,
suggested fix/tests, and status. The diff manager supports preview, approve,
reject, request revision, inspect checks, edit commit message, create commit,
push, and PR creation.

## 10. API Analyzer

Extract endpoint ID, HTTP method, route, controller and handler symbols,
request/response DTOs, auth metadata, parameters, response metadata, source
location, and confidence.

Resolve framework mapping annotations, method and path, handler, DTOs, and
service calls. Initial target is Spring MVC; later adapters may support WebFlux,
Express, NestJS, and FastAPI. Static analysis does not guarantee discovery of
all runtime-generated routes.

## 11. AI Agent Runtime

### Context builder

Collect selected nodes, related source, calls, references, dependencies, API
details, stack, and project rules. Rank relevant context, remove duplicates, and
fit the model context window. Never send the whole project by default.

### Review agent

Review correctness, security, performance, maintainability, architecture, error
handling, concurrency, resource management, and testability. Link every finding
to evidence.

### Patch agent

For approved findings, identify affected files, constrain the change, generate
the patch and tests, and produce a diff. Do not fix unrelated issues. Explain
any necessary supporting changes.

### Provider adapter

Support OpenAI, Anthropic, local models, and compatible providers through a
common adapter for calls, structured output, errors, usage, context limits, and
retry policy.

## 12. Review Findings and State

Finding fields: ID, project/snapshot IDs, selected scope, title, description,
category, severity, confidence, evidence locations, affected symbols, suggested
fix/tests, and status.

Severity: Critical, High, Medium, Low, Informational.

Lifecycle: Detected → Presented → Approved or Rejected → Patch Planned → Patch
Applied → Verification → Verified → Commit Ready → Committed. Verification may
fail. Bind findings to a snapshot because results can change as code changes.

## 13. Patching and Git Transactions

Prefer one finding → one fix task → one isolated workspace → one validated patch
→ one commit. A fix may change multiple files. Merge findings only when
technically inseparable, explain why, and obtain approval.

Use one Git worktree and branch per fix task. Never modify uncommitted files in
the original worktree. If the user asks to analyze uncommitted changes, use a
safe copy.

Patch lifecycle: create worktree; generate patch; verify; show diff; obtain
approval; commit; queue integration; optionally push/create PR. On rejection,
discard or archive the worktree.

Verification checks, when available:

1. Patch applies and touched files are in scope.
2. Build or compile.
3. Lint and static checks.
4. Existing and new tests.
5. Relevant regression tests.
6. Diff against the approved scope.
7. Full verification after integration.

Detect project tooling and commands; do not assume one command for all projects.
Limit retries. On failure, collect logs, report the cause, and never mark
unavailable or skipped checks as passed.

Associate each commit with finding/task IDs, changed files, check results, SHA,
timestamp, and approval. Integrate validated commits in order, check conflicts,
and reverify the final branch. Preserve the last known good state if integration
fails.

Remote push and PR creation require explicit user approval. PR summaries may
include purpose, findings, changed files, tests, risks, and unverified items.

## 14. Verification Engine

Run policy checks, build/compile, lint/static checks, unit, integration and
regression tests, then validate the diff. Select commands from detected project
configuration. For Kotlin projects, Gradle tasks such as compileKotlin, test, or
build are candidates, but are not required by the MVP.

Result fields: task ID, overall status, build/lint/test/regression/diff
statuses, command results, errors, warnings, and duration. Status: Passed,
Failed, Skipped, Unavailable, or Cancelled. Skipped and Unavailable are not
Passed.

## 15. Local Persistence

Use SQLite for Projects, ProjectSnapshots, Files, GraphNodes, GraphEdges,
ApiEndpoints, AnalysisJobs, ReviewSessions, ReviewFindings, FixTasks,
VerificationResults, GitTransactions, AgentExecutions, and Settings.

Store project path/config/stack; snapshot HEAD, hashes and analyzer version;
graph entities; review history; fix state and worktree; commit SHA, branch and
integration result; and AI execution status, model, scope, result and errors.
Never store secrets or tokens in ordinary logs.

## 16. Internal Tauri Commands

The UI must not execute filesystem or Git operations directly. Use typed
Rust/Tauri commands with explicit inputs, errors, permissions, and async
progress events.

- Project: open, scan, refresh, getStructure, getStatus.
- Graph: getNodes, getEdges, getDependencies, getCallers, getCallees,
  getExecutionFlow.
- API: list, get, getCallFlow.
- Review: create, getFindings, approveFinding, rejectFinding.
- Fix: create, generatePatch, verify, getDiff, approve, reject.
- Git: createCommit, integrate, push, createPullRequest.

## 17. Security and Execution Safety

- Analyze only user-selected project roots; do not follow symlinks outside the
  approved root.
- Protect existing changes. Require approval for deletion or broad edits.
- Warn before sending code to external LLMs. Review and exclude or redact API
  keys, passwords, tokens, .env files, private keys, credentials, and configured
  private files.
- Treat project scripts and build commands as arbitrary code. Provide command
  allowlists, pre-run approval, timeouts, cancellation, logs, optional network
  restrictions, and sandboxing.
- A Git worktree isolates file changes but is not a security sandbox.
- Prevent direct edits to the default branch, unauthorized force-pushes, and
  unapproved remote operations. Check state before worktree cleanup.
- Validate AI claims against source. Reject findings citing nonexistent files or
  symbols.

## 18. Errors and Recovery

| Condition | Required behavior |
| --- | --- |
| Missing project path | Fail to open; explain the error |
| File access denied | Report and skip that file |
| Unsupported language | Keep file browsing; limit deep analysis |
| LSP unavailable | Fall back to AST analysis when possible |
| API route unresolved | Mark unknown |
| Provider failure | Retry or offer another provider |
| Invalid AI output | Fail schema validation |
| Patch failure | Report and allow bounded retry |
| Test failure | Block commit readiness |
| Worktree failure | Leave original repository unchanged |
| Git conflict | Stop integration and report |
| Network failure | Preserve local work |
| Push failure | Keep local commits |
| PR failure | Show remote branch status |
| App interruption | Restore persisted task state |

## 19. Incremental Performance

Hash files and reparse only changed files. Update affected symbols,
relationships, graph snapshots, and diagrams. Cache graph layout, prioritize by
project size, render visible nodes, group large graphs, deduplicate AI context,
run analysis asynchronously, support cancellation, and show partial results.

## 20. Analyzer Interfaces

Language adapters may implement parseFile, extractSymbols, extractImports,
resolveDefinitions, findReferences, extractCallRelations, and
extractControlFlow.

Framework adapters may implement detectFramework, extractEndpoints,
resolveDependencyInjection, resolveRequestResponseTypes, and
resolveFrameworkRelations.

Report capability and confidence per feature; analyzers need not provide equal
accuracy. Initial adapters: Kotlin and Spring Boot; future adapters: Java,
TypeScript, JavaScript, Python, Express, NestJS, and FastAPI.

## 21. Suggested Source Layout

    visual-code-harness/
    ├── apps/desktop/
    │   ├── src/{components,features,stores,types,services}/
    │   │   └── features/{project-explorer,graph-viewer,api-explorer,review-panel,diff-viewer,git-manager}/
    │   └── src-tauri/src/{commands,state,events}/
    ├── crates/
    │   ├── harness-core/{orchestrator,scheduler,workflow}/
    │   ├── project-scanner/{discovery,detection,git-status}/
    │   ├── code-intelligence/{ast,lsp,symbols,call-graph,flow-analysis}/
    │   ├── framework-analyzers/{spring,express,nestjs,fastapi}/
    │   ├── knowledge-graph/{nodes,edges,queries}/
    │   ├── agent-runtime/{context,review,patch,providers}/
    │   ├── verification/{build,test,lint,policy}/
    │   ├── git-manager/{worktree,commit,integration,remote}/
    │   └── persistence/{sqlite,migrations}/
    ├── tests/{fixtures,integration,end-to-end}/
    ├── docs/{architecture,specifications,protocols}/
    ├── Cargo.toml
    └── package.json

This is a starting point; adjust crate boundaries to actual project size.

## 22. Functional Requirements

Each ID is independently traceable; semicolons separate requirements.

- **FR-01 Project:** 01 register local directory; 02 browse files; 03 detect
  language/framework; 04 inspect Git state.
- **FR-02 Analysis:** 01 parse AST; 02 extract classes/functions; 03 analyze
  references/calls; 04 extract module dependencies; 05 link results to source
  locations.
- **FR-03 Visualization:** 01 show project structure; 02 architecture diagrams;
  03 API route diagrams; 04 dependency graphs; 05 execution-flow graphs; 06
  select nodes; 07 export Mermaid.
- **FR-04 Review:** 01 review selected scope; 02 show severity; 03 link
  evidence; 04 suggest fixes; 05 approve/reject findings individually.
- **FR-05 Fixes:** 01 create tasks for approved findings; 02 isolate from
  original worktree; 03 generate patches; 04 show diffs; 05 verify; 06 safely
  stop/retry failures.
- **FR-06 Git:** 01 worktree per fix; 02 independent commits; 03 integrate
  changes; 04 detect conflicts; 05 push; 06 create PRs.
- **FR-07 Tasks:** 01 show task status; 02 cancel long-running work; 03 inspect
  errors/logs; 04 restore state after restart.

## 23. Non-Functional Requirements

- **NFR-01 Reliability:** AI failures must not damage source files; restore
  persisted tasks/commits after interruption.
- **NFR-02 Security:** User controls external code transfer; restrict sensitive
  data and command execution.
- **NFR-03 Extensibility:** Add language/framework analyzers and AI providers
  independently.
- **NFR-04 Performance:** Skip unchanged files; update affected graph regions
  only; support cancellation and partial results for large projects.
- **NFR-05 Traceability:** Link each AI edit to its finding, changed files,
  verification, and commit.
- **NFR-06 User control:** Never overwrite in-progress original files; disclose
  out-of-scope edits; require approval for remote Git operations.

## 24. End-to-End Example

A user opens a Kotlin / Spring Boot project. The app detects Kotlin, Spring
Boot, Gradle, and Git. The user explores the graph, selects UserService, and
runs a review. Findings include an N+1 query, missing exception handling, and a
transaction boundary issue. The user approves three fixes. Each gets an isolated
task, verification, diff review, and separate commit. The app integrates
commits, checks conflicts, runs full tests, shows the final diff, and offers
push or PR creation for approval.

This example is illustrative; the project and findings are not guaranteed
analyzer output.

## 25. Product Definition

The product combines local project understanding, visual exploration,
evidence-backed AI review, isolated refactoring, verification, and atomic Git
workflows in one desktop interface. Its purpose is to help users understand
project structure and guide AI development work through visual interaction.
