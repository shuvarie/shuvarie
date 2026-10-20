# Changelog

## [0.5.0] - 2026-10-20

### 🚀 Features

- Rhai-based codemode (#101)
- *(session)* Seamless turn resume after failure
- *(tui)* Add peak/off-peak display
- *(tui)* Add peak table

### 🐛 Bug Fixes

- *(screensaver)* Entrance lengths
- *(codemode)* Rhai code gotcha
- *(codemode)* Hardened rhai
- *(codemode)* Patch rhai skill

### 🎨 Styling

- *(tui)* Use model display name for label
## [0.4.3] - 2026-10-19

### 🐛 Bug Fixes

- *(session)* Keep last turn messages after interruption
## [0.4.2] - 2026-10-15

### 🚀 Features

- *(tui)* Multi-line display for `run_shell` tool title

### 🐛 Bug Fixes

- *(tui)* Mispositioned markdown table rendering
## [0.4.1] - 2026-10-15

### 🚀 Features

- *(command)* Add guardrail for invalid slash commands

### 🐛 Bug Fixes

- *(question)* Make escape interrupt turn

### 🎨 Styling

- *(art)* Use embedded art instead
## [0.4.0] - 2026-10-13

### 🚀 Features

- *(command)* Add todo list command
- *(tui)* Add turn labels of overall time taken and model names (#98)
- *(notification)* Add on-finish notification
- *(image)* Add image pasting from clipboard
- *(headless)* Add headless mode
- *(tui)* Update permission folder key
- *(tui)* Add waiting state for queued tools
- *(tui)* Welcome (onboard) screen rework (#99)

### 🐛 Bug Fixes

- *(tui)* List item scrolling should move selected area
- *(question)* No scrolling for question dialog

### 📚 Documentation

- *(skill)* Update rig skill

### 🚜 Refactor

- After frame logic

### 🎨 Styling

- *(spinner)* Update block spinner frames
- *(status)* Add initializing status

### ⚙️ Miscellaneous Tasks

- *(formatting)* Update formats
## [0.3.3] - 2026-10-08

### 🐛 Bug Fixes

- *(notify)* Notify keeps triggering update
- *(session)* Assistant prompts ignored in reloaded session

### ⚡ Performance

- *(alloc)* Use mimalloc for musl build
- *(notify)* Lower notify poll rate to once per 5 seconds

### 🧪 Testing

- Update core_task mock
- Add prompt history tests
## [0.3.2] - 2026-10-08

### 🐛 Bug Fixes

- *(session)* Preserve assistant prompt when retry
- *(session)* Token exceed limit
## [0.3.1] - 2026-10-06

### 🐛 Bug Fixes

- *(llm)* Query generic OpenAI-compatible servers over Chat Completions (#97)
## [0.3.0] - 2026-10-04

### 🚀 Features

- *(compaction)* More lax compaction triggers (#78)
- [**breaking**] Add model code-based `assisted-by` generation (#80)
- *(config)* [**breaking**] Add back boolean argument for `enabled`/`disabled`
- *(db)* Add global store support (#84)
- *(registry)* Custom registry support (#88)
- *(scene)* Add builtin scenes / scene tool concurrency
- Replace councillor scene with orchestrator
- *(theme)* Add builtin themes / rework theme system
- *(permission)* Add allow directory
- Image and document attachments and reading (#94)
- *(permission)* Add allowed builtin rules
- *(git)* Listen to HEAD file for branch update
- *(decision-model)* Add decision model provider support
- *(scrolling)* Add emacs-like half-screen scrolling

### 🐛 Bug Fixes

- *(update)* Failed to fetch under proxy
- *(db)* Wal file opening error preventing startup
- *(tui)* Exit on welcome screen not effective
- *(tui)* Ctrl modifier in tmux
- *(key)* Remove `j`/`k` for up and down

### 📚 Documentation

- *(agents)* Add test code notice
- *(agents)* Update test code notice
- *(agents)* Update AGENTS.md
- *(agents)* Fix typo
- *(agents)* Update design docs
- *(agents)* Remove model listing
- *(agents)* Update decision model description

### ⚡ Performance

- *(sidebar)* Cache context display

### 🚜 Refactor

- Update stale test cases
- [**breaking**] Port to rig 0.43 and selune 0.4

### 🎨 Styling

- *(spinner)* Update wait spinner
- *(tui)* Use styled popups by `tui-popup`
- *(tui)* Make chat scrollbar based on `tui-scrollbar`

### 🧪 Testing

- Remove stale/unreproducible test cases
- Use temp directory instead
- Optimize RAM usage of test cases
- Trim out context window test case due to strong coupling
- Replace hard-coded data

### ⚙️ Miscellaneous Tasks

- Run tests on entire workspace with one character indication
- Lint against entire workspace
- Add cargo publish
- Changelog dedup with shell script
## [0.2.4] - 2026-10-03

### 🐛 Bug Fixes

- *(session)* Retry wiping out assistant prompts
## [0.2.3] - 2026-09-30

### 🐛 Bug Fixes

- *(session)* Retry would delete assistant prompts
## [0.2.2] - 2026-09-23

### 🐛 Bug Fixes

- *(tui)* No word wrap for question block

### ⚙️ Miscellaneous Tasks

- *(cliff)* Use branch tags
## [0.2.1] - 2026-09-22

### 🚀 Features

- Add generic turn retry

### 🐛 Bug Fixes

- *(store)* Store loaded even not used
- *(tui)* Unable to navigate command popup
- *(tree)* Walk after assistant/tool prompt node when selected instead

### 🚜 Refactor

- *(core)* Split code in core_task
## [0.2.0] - 2026-09-22

### 🚀 Features

- *(cli)* Update cli arguments
- *(cli)* Add self update
- *(provider)* Provider expansion / add oauth2 login (#66)
- *(tui)* Skill tool block with no output when collapsed
- *(provider)* Update oauth2 login
- Add title generation (#69)
- *(config)* [**breaking**] Make `disabled` nodes switches
- *(update)* Make self-update aware of musl build
- *(websearch)* [**breaking**] Use DuckDuckGo Lite by default
- Add chat search
- Add stdio and mcp tools (#75)
- *(scenes)* Make default scene switchable / add default interlude
- *(title)* Rework configuration / add `gen-title` command
- *(command)* Add custom commands

### 🐛 Bug Fixes

- *(tui)* Misinterpreted keys in tmux
- *(scenes)* Make interludes truly only used during mid-session switch
- *(windows)* Compile error when importing

### ⚙️ Miscellaneous Tasks

- Allow publishing release with git-cliff changelog
- Remove inaccurate changelog
- Add musl build
## [0.1.1] - 2026-09-18

### 🚀 Features

- *(tui)* Add git branch update for each turn

### 🐛 Bug Fixes

- *(question)* Custom answer selection / editing
