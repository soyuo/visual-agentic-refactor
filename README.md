# Visual Agentic Refactor

로컬 코드베이스를 시각적으로 탐색하고, 근거 기반 AI 리뷰와 안전한 자동 수정
작업을 지원하는 Tauri 데스크톱 도구입니다.

## 1. 프로젝트 소개

Visual Agentic Refactor는 프로젝트 파일, 심볼, API, 의존성, 호출 관계를 분석해
Code Knowledge Graph로 보여줍니다. 사용자는 원하는 파일이나 그래프 노드를
선택해 AI 리뷰를 요청하고, 소스 위치가 연결된 finding을 확인한 뒤 필요한 수정만
승인할 수 있습니다.

자동 수정은 원본 작업 트리가 아닌 격리된 Git worktree에서 수행합니다. diff와
검증 결과를 확인한 뒤에만 commit, integration, push 또는 PR 작업을 진행합니다.

## 2. 프로젝트 제작 배경

대규모 프로젝트를 이해하거나 수정할 때 파일을 직접 검색하고 AI에게 전체
프로젝트를 전달하는 방식에는 다음 문제가 있습니다.

- 코드 구조와 의존 관계를 한눈에 파악하기 어렵습니다.
- AI의 설명이 실제 소스 위치와 연결되지 않을 수 있습니다.
- 전체 프로젝트를 전달하면 불필요한 정보와 민감한 파일이 포함될 수 있습니다.
- 자동 수정이 원본 작업 트리를 훼손하거나 기존 변경 사항과 충돌할 수 있습니다.
- 검증되지 않은 수정이 곧바로 commit 또는 원격 저장소 작업으로 이어질 수 있습니다.

이 프로젝트는 분석기가 확인 가능한 사실을 수집하고, AI는 선택된 범위와 분석
근거를 해석하도록 역할을 나눕니다. 외부 전송, 수정, commit, 원격 Git 작업은
각각 사용자의 승인을 거치며, 프로젝트 데이터와 자격 증명은 로컬을 기본으로
보호합니다.

## 3. 설치 및 개발 환경

### 요구 환경

- Node.js와 npm
- Rust toolchain
- Tauri 2 빌드 도구
- Git CLI (Git 기능 사용 시)
- Windows 개발 시 Visual Studio C++ Build Tools
- Claude Code 사용 시 Node.js/npm과 공식 `claude` CLI

### 설치 및 실행

```bash
cd apps/desktop
npm install
npm run tauri:dev
```

프론트엔드만 실행하거나 배포용 정적 자산을 빌드할 수 있습니다.

```bash
npm run dev
npm run build
npm run tauri:build
```

## 4. 기능과 기능의 동작 과정

### 프로젝트 탐색과 분석

1. 사용자가 로컬 프로젝트 루트를 선택합니다.
2. Rust 코어가 경로와 심볼릭 링크 경계를 검증합니다.
3. 파일 필터와 ignore 규칙을 적용해 파일을 스캔합니다.
4. Kotlin, Java, Spring, Gradle 및 확장 대상의 프로젝트 정보를 감지합니다.
5. 분석기가 심볼, API, 호출 관계, 의존 관계와 소스 위치를 추출합니다.
6. 분석 결과를 SQLite 스냅샷과 그래프 모델로 저장합니다.

### 그래프 탐색

SolidJS와 SVG로 프로젝트 구조, API, 의존 관계, 호출 관계를 표시합니다. 노드를
선택하면 관련 심볼과 소스 범위, 관계의 근거와 신뢰도를 확인할 수 있습니다.

### AI 리뷰

1. 사용자가 파일 또는 그래프 노드를 선택합니다.
2. 컨텍스트 빌더가 선택 범위, 관련 코드, 호출·참조, 의존성, API 정보를 수집합니다.
3. 사용자가 외부 전송을 명시적으로 승인합니다.
4. 선택된 provider adapter가 구조화된 리뷰 요청을 실행합니다.
5. 반환된 finding을 실제 분석 결과와 소스 위치에 대조합니다.
6. 검증된 finding을 리뷰 UI에 표시하고 사용자가 개별 승인 또는 거절합니다.

OpenAI와 Anthropic은 API key 경로를 사용하며, Claude Code는 제3자 OAuth가 아닌
로컬 공식 CLI를 사용합니다. Claude Code가 없으면 앱이
`npm install -g @anthropic-ai/claude-code`를 시도하고, 인증이 필요하면 공식
Claude Code 대화형 로그인으로 넘깁니다. 앱은 Claude Code 토큰, 쿠키 또는
private endpoint를 직접 읽지 않습니다.

### 안전한 패치와 Git 작업

승인된 finding마다 독립 수정 작업을 만들고 격리 worktree에서 패치 계획을
적용합니다. 변경 범위와 diff를 검사한 뒤 프로젝트 검증을 실행하고, commit
readiness가 승인된 작업만 순차적으로 통합합니다. 충돌이나 검증 실패가 발생하면
상태와 로그를 보존하고 사용자에게 보고합니다.

## 5. 사용자 작업 흐름

```text
프로젝트 열기
  → 파일 스캔 및 프로젝트 감지
  → 코드 분석 및 그래프 탐색
  → 파일/노드 범위 선택
  → 외부 전송 승인
  → AI 리뷰 및 근거 확인
  → finding별 수정 승인
  → 격리 worktree에서 패치
  → diff 및 검증 결과 확인
  → commit/integration 승인
  → push 또는 PR 작업 승인
```

## 6. 지원 범위

### 현재 제품 범위

- 데스크톱 앱: Tauri 2
- UI: SolidJS + TypeScript
- 네이티브 코어: Rust
- 로컬 저장소: SQLite
- 그래프: SolidJS + SVG
- 분석 방식: 정적 분석 및 Tree-sitter 기반 analyzer
- Git: 시스템 Git CLI
- AI: 공통 provider adapter 기반 OpenAI, Anthropic, 로컬 모델, Claude Code CLI
- 초기 분석 대상: Kotlin/Spring Boot

### 확장 범위

JavaScript/TypeScript, Python, 추가 프레임워크, 증분 분석, 대규모 그래프
최적화, 로컬 LLM, GitHub 연동, analyzer plugin을 확장 대상으로 둡니다.

분석 대상 프로젝트를 빌드하거나 실행하지 않으며, MVP에는 HTTP backend, JVM,
컨테이너, 외부 데이터베이스 또는 언어 서버가 필요하지 않습니다.

## 7. 만들면서 해결한 문제점들

- 전체 프로젝트를 AI에 전달하지 않고 선택 범위와 관련 근거만 구성했습니다.
- AI 결과가 실제 파일·심볼·위치와 일치하는지 검증하도록 했습니다.
- API key 인증과 subscription 인증을 별도 경로로 분리했습니다.
- Claude Code private token을 추출하는 대신 공식 로컬 CLI와 대화형 로그인을
  사용하도록 경계를 정했습니다.
- 원본 작업 트리의 기존 변경을 보존하기 위해 수정 작업을 격리했습니다.
- 패치 범위, diff, 검증 결과, commit readiness를 각각 확인하도록 했습니다.
- 실패·취소·재시작·충돌 상태를 저장하고 제한된 재시도와 복구 흐름을 마련했습니다.
- 대규모 그래프, 증분 분석, 추가 언어·프레임워크를 공통 analyzer 계약으로
  확장할 수 있도록 분리했습니다.

## 8. 검증과 제한 사항

### 검증 명령

```bash
npm run build
npm run test:graph
npm run test:review

cd apps/desktop/src-tauri
cargo fmt --all -- --check
cargo check
```

검증 결과는 통과, 실패, 생략, 사용 불가, 취소 상태를 구분해야 합니다. 현재
Windows 환경에서 Rust 전체 컴파일은 MSVC의 `cl.exe`와 `link.exe`가 필요합니다.

### 제한 사항

- 분석 결과는 정적 분석 범위에 한정되며 reflection이나 동적 등록은 미확정일 수
  있습니다.
- AI provider 사용 시 사용자가 외부 전송을 승인해야 합니다.
- API key와 Claude Code subscription 로그인은 각각 해당 provider의 환경과
  계정 상태에 의존합니다.
- Claude Code 인증 파일이나 토큰의 직접 조회는 지원하지 않습니다.
- Git CLI가 없으면 파일 탐색은 가능하지만 Git 기능은 사용할 수 없습니다.
- 프로젝트별 전체 플랫폼 실행 검증과 일부 대규모 프로젝트 성능 검증은 환경에
  따라 추가 확인이 필요합니다.

## 9. 문서 링크 및 기여 안내

- [제품 및 아키텍처 명세](./SPEC.md)
- [구현 진행 체크리스트](./STEP.md)
- [저장소 작업 지침](./AGENTS.md)
- [라이선스](./LICENSE)

기여하기 전 `AGENTS.md`와 관련 `SPEC.md` 섹션을 확인해 주세요. 기능 변경은
적절한 이슈와 브랜치에서 진행하고, 변경 범위를 작게 유지하며, 관련 검증 결과와
실행하지 못한 검사를 PR에 기록합니다. 이슈와 PR은 저장소 템플릿을 따르고,
원격 Git 작업은 별도 승인을 받아야 합니다.
