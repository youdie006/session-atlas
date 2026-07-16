<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/youdie006/sessionwiki/main/docs/banner-ko-dark.png">
  <img src="docs/banner-ko.png" alt="sessionwiki — 지금까지의 모든 AI 코딩 세션: 검색·연결·이어가기, AI 코딩 도구 11종. 100% 로컬.">
</picture>

<a href="https://github.com/youdie006/sessionwiki/actions/workflows/ci.yml"><img src="https://github.com/youdie006/sessionwiki/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="https://crates.io/crates/sessionwiki"><img src="https://img.shields.io/crates/v/sessionwiki?logo=rust&color=3b5bd6" alt="crates.io"></a>
<a href="https://github.com/youdie006/sessionwiki/releases/latest"><img src="https://img.shields.io/github/v/release/youdie006/sessionwiki?color=3b5bd6&label=release" alt="최신 릴리스"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT 라이선스"></a>

<a href="README.md">English</a> &middot; <b>한국어</b>

<a href="#설치"><img src="docs/nav/install.png" height="20" alt="설치"></a>
<a href="#빠른-시작"><img src="docs/nav/quick-start.png" height="20" alt="빠른 시작"></a>
<a href="#명령어"><img src="docs/nav/commands.png" height="20" alt="명령어"></a>
<a href="#코드를-만든-세션으로-거슬러-가기"><img src="docs/nav/trace.png" height="20" alt="추적"></a>
<a href="#아무것도-잃지-않기-아카이브-모드"><img src="docs/nav/archive.png" height="20" alt="아카이브"></a>
<a href="#멈춘-곳에서-다시-시작하기"><img src="docs/nav/resume.png" height="20" alt="이어가기"></a>
<a href="#어댑터-추가하기"><img src="docs/nav/add-a-tool.png" height="20" alt="어댑터 추가"></a>

<img src="docs/demo-cli.webp" width="760" alt="터미널 녹화: sessionwiki scan이 세 툴에 걸친 47GB 세션을 보여주고, search가 과거 대화를 키워드로 찾고, resume이 원래 툴에서 다시 엽니다">

</div>

**sessionwiki는 AI 코딩 툴들이 어차피 디스크에 남기는 흔적을 읽어, 실제로 다시 찾아갈 수 있는 하나의 검색·연결 가능한 아카이브로 만듭니다.** 데몬도, 클라우드도, 새로 들일 기록 습관도 필요 없습니다 &mdash; 모든 툴을 한 번에 인덱싱한 다음, 태그를 달고, 서로 연결하고, 멈춘 곳에서 다시 이어가게 해줍니다.

```console
$ sessionwiki scan
TOOL            SESSIONS       SIZE  OLDEST       NEWEST        PATH
claude-code         1763     1.1 GB  2026-03-27   2026-06-12    ~/.claude/projects
codex               2340    45.9 GB  2025-08-21   2026-06-12    ~/.codex/sessions
gemini                50     1.2 MB  2026-04-02   2026-06-10    ~/.gemini/tmp

4153 sessions across 3 tools, 47.0 GB on disk.
```

실제 머신 한 대의 결과입니다. 본인 머신에서 돌려보세요 &mdash; 숫자에 보통 놀랍니다. 3주 전에 Claude가 고쳐준 그 CORS 버그 대화도 아직 디스크에 있습니다. 못 찾을 뿐이죠: 툴마다 다른 포맷으로, 다른 폴더에, 쓰는 머신마다 제각각 기록하니까요. 몇 달이면 해결된 문제로 가득한 대화 수천 개가 쌓이는데, 다시 찾아갈 방법이 없습니다.

읽는 게 더 편하면 웹 UI도 있습니다 &mdash; `sessionwiki web`:

<img src="docs/demo-web.webp" width="820" alt="sessionwiki 웹 UI: 검색창 실제 타이핑, 태그·resume 명령이 달린 트랜스크립트, 파일 프로비넌스, 태그 필터, 다크 모드">

## 무엇을 할 수 있나

- **찾기** &mdash; 이 머신의 모든 세션 저장소: 어떤 툴이, 어디에, 몇 개, 몇 GB &mdash; 즉시.
- **검색** &mdash; 모든 툴의 모든 메시지를 한 번에. 부분 문자열 매칭이라 식별자 조각도, 한국어·일본어·중국어도 별도 설정 없이 검색됩니다.
- **읽기** &mdash; 코드 블록 렌더링, 접힌 툴 호출, 긴 세션의 목차까지 갖춘 깔끔한 트랜스크립트.
- **요약** &mdash; 본인 LLM CLI로 세션마다 한두 문장 시놉시스를 만들어 캐시 &mdash; "이 세션 뭐였지"가 사라집니다.
- **이어가기** &mdash; 명령 하나로 원래 툴에서, 원래 프로젝트 디렉토리에서 세션을 다시 엽니다.
- **컨텍스트 이식** &mdash; Claude Code 세션을 Codex로, 어디로든 브리핑해서 넘깁니다.
- **파일에서 세션 거슬러 가기** &mdash; `trace src/auth.rs`는 한 파일을 편집한 AI 대화들을 툴 가로질러 보여줍니다. 세션과 그것이 만든 코드의 연결. [프로비넌스](#코드를-만든-세션으로-거슬러-가기) 참고.
- **줄 단위로 거슬러 가기** &mdash; AI 시대의 `git blame`. `blame src/auth.rs`가 git 히스토리와 인덱스를 결합해 각 줄 뒤의 AI 세션을 짚어냅니다. best-effort 휴리스틱이고 저작 증명은 아니며, 파일 단위 `trace`로 폴백합니다.
- **툴이 지운 것까지 보존** &mdash; 툴이 prune한 세션을 [아카이브](#아무것도-잃지-않기-아카이브-모드)해, 오래된 작업에도 검색·추적이 꺼지지 않습니다.

## 설치

**Homebrew** (macOS / Linux):

```console
brew install youdie006/tap/sessionwiki
```

**빌드된 바이너리** (툴체인 불필요). macOS / Linux / WSL:

```console
curl -sSL https://raw.githubusercontent.com/youdie006/sessionwiki/main/scripts/install.sh | sh
```

**Windows** (네이티브 PowerShell):

```powershell
irm https://raw.githubusercontent.com/youdie006/sessionwiki/main/scripts/install.ps1 | iex
```

각 스크립트가 플랫폼에 맞는 아카이브를 [최신 릴리스](https://github.com/youdie006/sessionwiki/releases/latest)에서 받아 체크섬을 검증하고 바이너리를 설치합니다(`~/.local/bin`, Windows는 `%LOCALAPPDATA%\Programs\sessionwiki`). **WSL은 리눅스**이므로 위 셸 한 줄을 쓰면 됩니다 &mdash; 리눅스 바이너리가 설치되고 WSL 홈의 세션 저장소를 읽습니다.

**Rust(stable)** 가 있다면:

```console
cargo install sessionwiki
```

어느 쪽이든 런타임 의존성 없는 단일 바이너리입니다.

### Claude Code 플러그인 (장기 기억)

Claude Code에 프로젝트의 장기 기억을 더합니다: SessionStart 훅이 과거 세션을
자동으로 회상하게 만듭니다. 먼저 위에서 `sessionwiki` CLI를 설치한 뒤, 이
레포에서 플러그인을 추가하세요:

```console
/plugin marketplace add youdie006/sessionwiki
/plugin install sessionwiki@sessionwiki-marketplace
```

이제 작업을 시작할 때 Claude가 이전 작업을 끌어오고, `/sessionwiki:recall
<주제>` 로 필요할 때 히스토리를 검색합니다. 플러그인은 로컬 `sessionwiki`
바이너리를 호출할 뿐이라 &mdash; 완전히 오프라인입니다. 바이너리가 `PATH`에
없으면 플러그인은 우아하게 비활성화되고 Claude는 회상 없이 그대로 동작합니다.

### MCP 서버 (모든 에이전트)

툴을 가로지르는 세션 히스토리를 MCP 클라이언트가 호출할 수 있는 도구로 노출합니다
&mdash; `search_sessions`, `trace_file`, `get_session_brief`. 전부 읽기 전용이고
100% 로컬(stdio, 소켓 없음)입니다.

```console
claude mcp add sessionwiki -- sessionwiki mcp
```

Cursor는 `~/.cursor/mcp.json`에 추가:

```json
{ "mcpServers": { "sessionwiki": { "command": "sessionwiki", "args": ["mcp"] } } }
```

## 빠른 시작

```console
sessionwiki scan                # 내 세션이 어디에 있지?
sessionwiki search "jwt retry"  # 모든 툴을 가로지르는 전문 검색
sessionwiki show 3f9c           # 찾은 대화 읽기
sessionwiki web                 # 또는 로컬 웹 UI로 전부 둘러보기
```

첫 `search`/`list`가 인덱스를 만듭니다. 기록 1GB당 몇 분 정도 걸리는 일회성 비용이고(Codex 헤비유저는 수십 GB일 수 있습니다), 이후 갱신은 증분이라 몇 초면 끝납니다.

## 명령어

| 명령 | 설명 |
|---|---|
| `scan` | 이 머신의 세션 저장소를 발견. 파일시스템만 훑으므로 즉시 끝납니다. |
| `list` | 모든 툴의 최근 세션을 한 타임라인으로. `--tool codex`, `--project api`, `--tag spike`, `-n 50`, `--all`(서브에이전트 트랜스크립트 포함). |
| `search <검색어>` | 모든 툴의 모든 메시지를 전문 검색. 최소 3글자. |
| `recall <검색어>` | 검색하고, 매치를 나열하고, 1순위를 한 명령으로 브리핑 &mdash; 과거 세션으로 가장 빠르게 복귀하는 길. `--tool`, `--project`, `-n`, `--json`(에이전트용). |
| `show <id>` | 세션 하나를 읽기 좋은 트랜스크립트로. `--full`은 툴 호출 전체 표시, `--json`은 파싱 결과 출력, `--outline`은 다이제스트(내가 던진 질문 전부 + 어떻게 끝났는지). |
| `summarize [id]` | **본인 LLM CLI**(기본 `claude -p`, `--cmd`/`SESSIONWIKI_SUMMARIZER`로 교체)로 1~2문장 시놉시스를 생성해 인덱스에 캐시하고 `show`·`--outline`·웹 사이드바에 표시. id 없이 실행하면 최근 `--recent N`개 일괄 처리. |
| `resume <id>` | 원래 툴에서 세션 다시 열기: `claude --resume` / `codex resume`를 해당 프로젝트 디렉토리에서 실행. 서브에이전트 트랜스크립트는 부모 세션을 엽니다. `--print`는 명령만 출력. |
| `migrate <id> <dir>` | 세션을 다른 프로젝트 디렉토리에서 resume할 수 있게 만듭니다: Claude Code는 트랜스크립트를 `<dir>`의 스토어로 복사, Codex는 어느 디렉토리에서나 id로 resume, Gemini는 채팅을 복사. 원본은 건드리지 않습니다. |
| `brief <id>` | 세션을 마크다운 브리핑으로 출력(긴 세션은 머리·꼬리만, 중간 생략) &mdash; 어느 툴로든, 툴을 바꿔서도 컨텍스트를 들고 갈 수 있습니다. `--max-chars`, `--tools`. |
| `web` | `127.0.0.1:7575` 로컬 뷰어: 날짜별 세션 목록, 하이라이트 스니펫 실시간 검색, 목차·태그·관련 세션이 달린 트랜스크립트, resume 명령, 라이트/다크, UI 자동 현지화(ko/en/ja/zh). 기존 인덱스를 읽고 `web --sync`로 먼저 갱신. localhost 밖으로 절대 나가지 않습니다. |
| `sync [--tool]` | 인덱스를 직접 빌드/갱신. 아래 `--no-sync`와 함께 쓰면 쿼리가 저장소 순회를 건너뜁니다. cron으로 인덱스를 미리 데워둘 때 유용. |

모든 쿼리 명령(`search`, `list`, `recall`, `show`, `brief`, `resume`, `trace`)은 `--no-sync`를 받습니다 &mdash; 저장소를 다시 훑지 않고 이미 빌드된 인덱스만 조회하는 빠른 경로로, 다른 무언가(예: `sessionwiki sync`를 도는 cron)가 인덱스를 최신으로 유지할 때 씁니다.

### 세션 엔지니어링

세션은 하나의 컨텍스트 단위이고, 수백 개가 쌓이면 검색만이 아니라 큐레이션과 관리가 필요해집니다. 아래 명령은 평평한 아카이브를 탐색 가능하고 정돈된 것으로 바꿉니다. 인덱스를 읽으므로 즉시 끝납니다.

| 명령 | 설명 |
|---|---|
| `related <id>` | 같은 주제의 세션: 같은 프로젝트 우선, 그다음 같은 파일을 편집한 세션, 그다음 태그를 공유하는 것. 내 작업의 "관련 항목". |
| `files <id>` | 세션이 편집·생성한 파일 목록 &mdash; 프로비넌스 링크의 세션 쪽. |
| `trace <경로>` | 한 파일을 건드린 AI 세션들(최신순). 상대 경로를 디스크의 절대 경로와 매칭하므로 `trace src/auth.rs`로 충분. [아래](#코드를-만든-세션으로-거슬러-가기) 참고. |
| `blame <경로>` | AI 시대의 git blame: 각 줄을, 그 줄을 마지막으로 바꾼 커밋 뒤의 AI 세션에 귀속(`git blame` + 인덱스 결합). `-L 40,80`으로 범위 지정. 저작 증명이 아닌 best-effort &mdash; `ambiguous`/`unattributed`는 정상이며 파일 단위 `trace`로 폴백. |
| `tag <id> <태그>...` | 세션에 태그(`--rm`으로 제거). id 없이 실행하면 전체 태그 목록. `list --tag`로 필터. 태그는 인덱스에 저장되고 재인덱싱에도 살아남으며, 원본 세션 파일은 절대 건드리지 않습니다. |
| `note <id> "텍스트"` | 세션에 자유 메모를 답니다. 텍스트를 생략하면 기존 메모를 읽습니다. |
| `forget <id>` | 세션을 인덱스와 아카이브에서 영구 삭제. [아카이브 모드](#아무것도-잃지-않기-아카이브-모드)에서 보존된 세션을 정말 지우고 싶을 때의 탈출구. |
| `projects` | 프로젝트별 한 줄: 세션 수, 메시지 양, 마지막 활동. 코드베이스마다 한 페이지. |
| `stats` | 전체 합계 + 툴별·월별 분포 + 연결된 파일 수 + 툴이 지운 뒤 보존된 세션 수. |
| `digest [--since 7d]` | 최근 세션을 프로젝트별로 묶은 마크다운 롤업 &mdash; 무엇을 작업했고, 각 세션이 건드린 파일, 캐시된 시놉시스까지. `--since 2w`/`24h`/`90m`, `--project`, `--tool`, `--json`. 스탠드업·PR 본문·"이번 주에 뭐 냈지" 뷰를 인덱스에서 조립. |

### 코드를 만든 세션으로 거슬러 가기

이제 코드는 대부분 AI가 씁니다. 그래서 질문은 "이 줄을 누가 썼나"가 아니라
"어느 대화가 만들었고 왜 그랬나"입니다. sessionwiki는 각 세션의 툴 호출에서
파일 편집을 읽어내고(Claude의 `Edit`/`Write`, Codex의 `apply_patch`) 모든
세션을 그것이 바꾼 파일에 연결합니다.

```console
$ sessionwiki trace src/middleware/mod.rs
2 session(s) touched "src/middleware/mod.rs", newest first:
35a59790  claude-code  2026-06-09  Fix CORS preflight failing on /auth routes
4fd0ce37  claude-code  2026-06-08  Add retry with backoff to the payment webhook handler
```

훅도 설정도 없이, 이미 디스크에 있는 모든 세션에 대해 소급으로 작동합니다.
정직하게 말하면 이건 한 파일을 *건드린* 대화로 안내하는 것이지 줄 단위 저작
주장이 아닙니다 &mdash; 이후 편집이 그 코드를 덮었을 수 있으니, `trace`는 관련
논의로 돌아가는 길입니다. 웹 UI에서는 세션이 건드린 파일이 헤더에 칩으로
나오고, 클릭하면 그 파일을 건드린 다른 세션들이 보입니다.

### 아무것도 잃지 않기 (아카이브 모드)

Claude Code와 Codex는 오래된 세션을 시간이 지나면 지웁니다. 분명히 AI가 만든
파일인데 그 세션이 삭제돼 `trace`가 빈 결과를 내면, 연결 자체가 무용지물입니다.
그래서 sessionwiki는 한 번 인덱싱한 세션은 보관합니다: 툴이 원본을 지우면 그
세션은 삭제가 아니라 **아카이브**되고, `search`·`trace`·`brief`가 계속 작동합니다.

```console
$ sessionwiki list          # Claude가 오래된 세션을 지운 뒤
archived 1 session(s) the tool removed (1 kept that your tools have deleted)
...
a1b2c3d4  claude-code  3w ago  12  …/api-server  Fix CORS preflight…  [archived]
```

자동·무음이고(기억해야 할 복사 단계 없음), 비용도 거의 없으며(증류된 트랜스크립트는
이미 인덱스에 있음), 되돌릴 수 있습니다: `forget <id>`로 영구 삭제, 디스크에 다시
나타난 세션은 자동으로 아카이브 해제. 원본 툴로 다시 열 수는 없지만, 읽고 `brief`로
새 세션에 넘기고 코드를 거슬러 추적하는 건 됩니다. 생성 시점 훅으로는 못 하는
부분입니다 &mdash; 이미 존재하는 수천 개의 세션, 그리고 당신이 안 볼 때 툴이 이미
지워버린 세션까지 작동하니까요.

## 멈춘 곳에서 다시 시작하기

옛 세션을 찾는 건 절반이고, 나머지 절반은 이어가는 것입니다.

```console
$ sessionwiki search "rate limiter"
76a614028a63 codex 2026-06-11 13:00 .../projects/api-server [assistant]
  ...the bucket invariant 0 <= tokens <= capacity holds after every step...

$ sessionwiki resume 76a6           # 그 대화를 Codex에서 다시 엽니다

$ sessionwiki brief 76a6 | claude -p \
    "이 작업 이어서: 빠진 엣지 케이스 테스트 추가해줘"

$ sessionwiki summarize --recent 20  # 최근 세션들에 시놉시스 달기
```

`resume`은 각 툴의 네이티브 기능을 쓰므로 원본 세션 파일이 남아 있어야 합니다. `brief`는 툴을 바꿔도 동작합니다. `summarize`는 당신의 LLM을, 당신의 머신에서, 당신이 시킬 때만 돌립니다 &mdash; sessionwiki 자체는 네트워크 호출을 하지 않습니다.

## 동작 원리

```mermaid
flowchart LR
    subgraph stores["이미 디스크에 있는 것"]
        A["~/.claude/projects"]
        B["~/.codex/sessions"]
        C["~/.gemini/tmp"]
    end
    A & B & C --> AD["어댑터<br>(툴당 작은 파일 하나)"]
    AD --> IDX[("SQLite FTS5 인덱스<br>trigram 토크나이저")]
    IDX --> CLI["CLI<br>scan / list / search / show<br>summarize / resume / brief"]
    IDX --> WEB["웹 뷰어<br>127.0.0.1 전용"]
```

- `scan`은 파일시스템만 훑고 인덱스를 건드리지 않습니다.
- 나머지는 `~/.local/share/sessionwiki/index.db`(플랫폼별 위치, `SESSIONWIKI_DATA`로 변경)에 증분 인덱스를 유지합니다. 수정시각·크기가 바뀐 파일만 다시 파싱합니다.
- 원본 세션 파일은 절대 수정하지 않습니다 &mdash; 인덱스는 언제든 지워도 되는 캐시입니다. 캐시된 요약만은 스키마 업그레이드에도 의도적으로 살아남습니다. 인덱스 재구축은 싸지만, 기록 전체에 LLM을 다시 돌리는 건 비싸니까요.
- 잡음은 일부러 거릅니다: 반복되는 하네스 보일러플레이트와 거대한 툴 출력은 인덱스에서 제외해 검색 결과의 신호를 지킵니다.

## 프라이버시

세션에는 코드와 대화가 들어 있으니 기준은 단순합니다: 네트워크 호출 없음, 텔레메트리 없음, 인덱스는 로컬 저장, 원본은 읽기 전용. 코드가 작아서 grep 한 번이면 직접 확인할 수 있습니다.

## 지원 툴

| 툴 | 세션 저장소 | 상태 |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` (중첩 서브에이전트 트랜스크립트 포함) | 지원 |
| Codex CLI | `~/.codex/sessions/**/rollout-*.jsonl` | 지원 |
| Gemini CLI | `~/.gemini/tmp/*/chats/*.json` | 지원 |
| OpenCode | `~/.local/share/opencode/opencode.db` (SQLite; 레거시 `storage/**` JSON도) | 지원 |
| Cline, Roo Code, Kilo Code | VS Code `globalStorage/<ext>/tasks/<id>/` (파서 하나로 세 툴) | 지원 |
| gajae-code (& Pi) | `~/.gjc/agent/sessions/**/*.jsonl` | 지원 |
| Continue | `~/.continue/sessions/*.json` | 지원 |
| gptme | `~/.local/share/gptme/logs/<session>/conversation.jsonl` | 지원 |
| aider | 레포별 `.aider.chat.history.md` (마크다운) | 지원 |
| Cursor, Zed, ... | | 예정 &mdash; PR 환영 |

**오마이코드(OMC)·오마이코덱스(OmX)·오마이오픈에이전트(OmO)·lazyclaudecode/lazycodex
쓰시나요?** 이들은 Claude Code·Codex·OpenCode 위의 하니스라, 대화가 그 도구들의 스토어에
그대로 기록됩니다 &mdash; 그래서 sessionwiki가 별도 설정 없이 이미 인덱싱합니다. 세션의 프로젝트에
하니스의 오케스트레이션 디렉터리(`.omc`/`.omo`)가 남아 있으면 그걸로 태그를 붙여
(`list --tag oh-my-claudecode`) 필터할 수 있습니다 &mdash; 트랜스크립트가 아니라 파일시스템 신호라,
그 도구를 *언급*만 한 세션은 잘못 태깅되지 않습니다. (오마이코덱스는 그런 디렉터리가 없어 태깅 안 됨;
가재코드는 자체 어댑터를 가진 독립 에이전트.)

### sessionwiki의 자리

AI 세션 기록 탐색은 이미 활발한 분야입니다. 아래 도구들은 저마다 잘하는 게 있습니다 &mdash; sessionwiki의 베팅은 그중 누구도 하지 않는 한 가지, 대화를 그것이 만든 코드에 잇는 것입니다.

| | 잘하는 것 | sessionwiki가 더하는 것 |
|---|---|---|
| [Claudia](https://github.com/getAsterisk/claudia) | 잘 다듬어진 Claude Code GUI | 크로스툴, CLI _와_ 웹, 그리고 `trace`로 코드를 그 대화에 연결 |
| [SpecStory](https://specstory.com) | 작업하는 동안 대화 기록을 캡처 | 이미 가진 세션에 소급으로 작동 &mdash; 캡처 단계가 필요 없음 |
| [claude-code-log](https://github.com/daaain/claude-code-log) | 한 툴의 트랜스크립트를 HTML로 렌더링 | 모든 툴을 한 번에, 전문 검색, 그리고 프로비넌스 |
| [cass](https://github.com/Dicklesworthstone/coding_agent_session_search) | 빠른 크로스툴·크로스머신 검색 | 파일&rarr;대화 프로비넌스, 지워진 세션 아카이브, 큐레이션, 웹 UI |

**`trace <파일>`** 은 한 파일에서 그것을 편집한 AI 대화로 거슬러 갑니다 &mdash; 소급으로, 훅 없이, 모든 툴을 가로질러, [툴이 이미 지워버린 세션까지](#아무것도-잃지-않기-아카이브-모드). 생성 시점 훅은 이미 가진 세션에 대해 이걸 못 하고, 단일 툴 뷰어는 툴을 가로질러 못 합니다.

솔직한 트레이드오프: 한 툴만 다루는 전용 뷰어는 그 툴 하나에 대해서는 sessionwiki의 어댑터보다 더 툴 특화된 완성도를 갖습니다. sessionwiki의 베팅은 크로스툴 척추에 코드 프로비넌스를 더한 것 &mdash; [오늘 11개 툴](#지원-툴)이고 늘려가는 중이며, 어댑터가 [PR](#어댑터-추가하기)로 가장 도움받는 #1 항목입니다.

## 어댑터 추가하기

당신의 에이전트가 세션을 디스크에 남긴다면, sessionwiki에 들어올 자격이 있습니다. 어댑터는 메서드 4개를 구현한 작은 Rust 파일 하나입니다:

```rust
pub trait Adapter {
    fn name(&self) -> &'static str;               // "my-tool"
    fn root(&self) -> Option<PathBuf>;            // 세션이 있는 곳
    fn discover(&self) -> Vec<PathBuf>;           // 모든 세션 파일
    fn parse(&self, path: &Path) -> Result<Session>; // 관대하게; 깨진 줄은 건너뛰기
}
```

가장 작은 예시는 [`src/adapters/gemini.rs`](src/adapters/gemini.rs)(약 100줄)입니다. [`src/adapters/mod.rs`](src/adapters/mod.rs)에 등록하고 PR을 열어주세요. 파서는 깨진 입력에 절대 패닉하면 안 됩니다 &mdash; 세션 포맷은 툴 버전에 따라 변하므로, 방어적으로 파싱하고 읽을 수 있는 만큼만 반환하세요.

## 로드맵

- 더 많은 어댑터 &mdash; Cursor, Zed 등. PR로 가장 도움받는 부분([어댑터 추가하기](#어댑터-추가하기))
- `merge` &mdash; 여러 머신의 인덱스를 하나로 병합
- `clean` &mdash; 거대한 옛 세션 저장소의 디스크 안전 회수
- 플랫폼별 빌드 바이너리 배포

최근 출시: [프로비넌스](#코드를-만든-세션으로-거슬러-가기)(`trace`/`files`)와 [아카이브 모드](#아무것도-잃지-않기-아카이브-모드).

## 기여

이슈와 PR을 환영합니다. 지금 가장 가치 있는 기여:

1. 쓰시는 툴의 **어댑터** (위의 "어댑터 추가하기" 참조)
2. 툴 업데이트로 세션 스키마가 바뀌었을 때의 **포맷 수정**
3. 파싱에 실패하는 세션 파일 첫 몇 줄을 담은 **버그 리포트** (민감한 부분은 마음껏 지우고)

## 라이선스

[MIT](LICENSE). 상업적 사용 포함 어떤 용도로든 자유롭게 &mdash; 라이선스 고지만 유지해주세요.

<div align="center">
<br>

<a href="https://github.com/youdie006/sessionwiki/issues/new">버그 신고</a> &middot;
<a href="https://github.com/youdie006/sessionwiki/issues/new">어댑터 요청</a> &middot;
<a href="#로드맵">로드맵</a>

</div>
