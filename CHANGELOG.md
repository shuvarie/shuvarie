# Changelog

## [unreleased]

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
- *(tui)* Add git branch update for each turn
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

- *(question)* Custom answer selection / editing
- *(tui)* Misinterpreted keys in tmux
- *(scenes)* Make interludes truly only used during mid-session switch
- *(windows)* Compile error when importing

### 📚 Documentation

- Update AGENTS.md
- *(readme)* Update feature description

### ⚙️ Miscellaneous Tasks

- Allow publishing release with git-cliff changelog
- Remove inaccurate changelog
- Add musl build

### 💼 Other

- Update debug version display
- Introduce git-cliff
- Configure git-cliff
- Prepare for trunk-based development
- Replace gnu make with just
