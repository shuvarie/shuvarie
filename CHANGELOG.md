# Changelog
## [unreleased]

### 🚀 Features

- *(cli)* Update cli arguments
- *(cli)* Add self update
- *(tui)* Add git branch update for each turn
- *(provider)* Provider expansion / add oauth2 login (#66)
- *(tui)* Skill tool block with no output when collapsed
- *(provider)* Update oauth2 login

### 🐛 Bug Fixes

- *(question)* Custom answer selection / editing

### 📚 Documentation

- Update AGENTS.md

### 💼 Other

- Update debug version display
## [0.1.0] - 2026-09-17

### 🚀 Features

- Basic architecture
- Basic core and llm
- Add model selection ui
- Ui design
- Add session UI
- Text streaming and pricing display
- Use `tui-scrollview`
- Turso database
- Add ollama cloud support
- Add cli flag
- Tool calls
- Markdown/syntax highlighting
- Llm catalog
- *(llm)* Multi-agent orchestration
- *(agent)* Context loading
- *(agent)* Turso vector/full text search for RAG
- *(config)* Split connection config
- *(agent)* Diff review
- *(agent)* Persistent session records
- Multiline text area
- *(agent)* Replace run_command with run_shell
- *(core)* Remove default max turn limit
- *(session)* Update session deletion behavior
- *(lsp)* Add lsp support
- *(skills)* Add skills system
- *(rig)* Upgrade to rig 0.42
- Port provider to Selune
- Upgrade selune client
- *(tools)* Add glob tool
- *(tools)* Add todo tool
- *(tools)* Add webfetch
- *(config)* Migrate from TOML to KDL
- *(config)* Use serde for kdl
- *(tools)* Add question tool
- *(tools)* Add apply patch tool
- *(config)* New connection config format
- *(config)* Update connection config
- *(config)* Update connection config format
- *(tools)* Shell hardening
- *(tools)* Read/write hardening
- *(chat)* Update session chat handling
- *(tui)* Remove home screen
- *(tools)* Remove todo tool
- *(tui)* Remove total token showing
- *(tools)* Edit tool rework
- *(tools)* Edit tool hardening
- *(session)* UUID-based session id / session reopen
- *(permission)* Remove popup / permission rework
- *(config)* Local config support / config override / config priority
- *(agents)* Update AGENTS.md and skills loading
- *(tui)* Implement slash command
- *(command)* Add `quit`
- *(config)* Auto add .gitignore for `.shuvarie` directory
- *(agents)* Thinking rework
- *(session)* Add elapsed time
- *(tools)* Add back todo
- *(tui)* Show active todos under title bar
- *(agents)* Add auto retry
- *(slash)* Remove `resume` command
- *(slash)* Add `continue` command
- *(session)* Add session type and parent
- *(terminal)* Add xterm title
- *(tui)* Context display rework
- *(context)* Refine context calculation
- *(context)* Refined context compression
- *(skills)* Refine skills system
- *(tui)* Add cursor movement for question answer area
- *(tui)* Clean text area with `ctrl+c`
- *(tui)* Update elapsed time display
- *(tui)* Add prompt steering (queuing)
- *(tui)* Move provider:model display to status bar
- *(tui)* Make prompt steering sent after next action rather than turn
- *(agents)* Update AGENTS.md loading
- *(command)* Add reload command
- *(tools)* Update default shell for shell tool
- *(command)* Add bash mode
- *(tui)* Exit code coloring for `run_shell` tool
- *(tui)* Collapsible sidebar
- *(tui)* Show read and cached token stats
- *(tui)* Make token stats persistent
- *(build)* Split config for debug and release builds
- *(tui)* Virtual clipboard paste / paste compact block
- *(tui)* Double Escape key for interruption
- *(tools)* Grep rework
- *(config)* Replace kdl-serde with custom se/deserializer
- *(tui)* Add path and git branch display
- *(tools)* Add back `apply_patch`
- *(tui)* Add block text area clearance
- *(tui)* Block display chunking and virtualizing
- *(tui)* Draft stack
- *(tui)* Virtual selection
- *(bash)* Popup output rework
- *(db)* Persist last session position
- *(config)* Add warning header to connections.kdl
- *(session)* Add title editing
- *(tui)* Provider and model selectors rework
- *(tool)* Web search
- Multi-client support
- *(tool)* Add `delete_file` tool
- *(permission)* Permission system rework
- *(permission)* Add mode / remove path shell pattern matching
- *(permission)* Remove builtin shell pattern
- *(permission)* Deny always interrupt turn
- *(tool)* Update `edit_file` tool
- *(trust)* Add trust system
- *(session)* Implement session tree
- *(tool)* Remove `continue` tool
- *(scene)* Add scene system
- *(cli)* Add directory argument
- *(keybinds)* Tree view and scene selector
- *(model)* Add model variant switch
- *(variant)* Add default cycle option and selector
- *(migration)* Use timestamp-based numbering
- *(themes)* Add theming support
- *(theme)* Add variant / light mode / mode detection
- *(session)* Add import/export feature
- *(tool)* Add skill tool
- *(permission)* Add allow for session button

### 🐛 Bug Fixes

- Use early break
- Session overlay update
- *(config)* Avoid config overwrite for integration test cases
- *(agent)* Partially generated turns
- Lsp-types
- *(agent)* AGENTS.md loading
- *(agent)* Broken per-call estimation and compaction
- Change max turn back to unlimited
- Tool call persistence
- *(tui/scrolling)* Selector scrolling
- *(channels)* Sender panic on exit
- *(tui)* Unstable scroll view
- *(catalog)* Use kind as connection type instead
- *(tools)* Diff format in edit file tool
- *(tui)* Freeze during tool calls
- *(tui)* Freeze during streaming
- *(tui)* Tui freeze during agent session
- *(tools)* Multiple todo blocks in tui
- *(tui)* Tui freeze
- *(steer)* Make steered prompts sent in the next action
- *(session)* Streamed text stitched together when saving to db
- *(tui)* Unexpected line breaks in markdown titles
- *(tui)* Interruption leaves unfinished block state
- *(tui)* Virtual selection display and key binds
- *(tool)* `apply_patch` often fails
- *(permission)* Local config overwrite other permissions
- *(session)* Text blocks stacked at bottom after loading session
- *(scene)* Local config loading failure
- *(session)* Session tree walking
- *(scene)* Scene reset for new session
- *(tree)* Session tree and undo during running session
- *(markdown)* Table cell line wrap
- No text wrap in question option
- *(tui)* Output display stacking and waiting
- *(scrolling)* Undo/tree select moved chat to top
- *(tree)* Undo/tree selection does not hide chats

### 📚 Documentation

- Format readme
- Add toasty skill
- Add ratatui skill
- Add rig skill
- Update AGENTS.md
- Update AGENTS.md
- Update AGENTS.md
- Update AGENTS.md
- Add rustdoc for `Event`
- Update toasty skill
- Update roadmap
- *(agents.md)* Remove duplicated list item
- Add in development notice
- Update roadmap
- *(skills)* Update rig skill
- *(tools)* Add roadmap
- *(skills)* Remove find-skills
- *(roadmap)* Remove opencode parity
- *(roadmap)* Remove
- Slim down AGENTS.md
- Update code convention in AGENTS.md
- Update readme
- Update readme
- Add demo screenshot
- *(agents)* Remove in development notice to freeze migrations
- *(readme)* Update description
- Add spec for readme
- Update spec
- Update contribution docs
- Update issur/pr section with more comprehensive words
- *(agents)* Make tips last
- Update AGENTS.md
- Add language conduct
- Clean up AGENTS.md
- *(agents)* Add convention section

### 🚜 Refactor

- TEA models
- Add inline to helper functions
- Remove unused imports
- Use `use` over qualifier
- Make modifier keys helper functions
- Update utils structure
- Architecture / event handling
- Update event error handling
- Unify app event mapping
- Format
- Make `map_event` instance method
- Make `map_event` instance method
- Rename `AppReturn` to `AppEffect`
- Isolate render loop logic
- Update run shell tool
- *(lsp)* Use workspace_folders for lsp workdir
- *(core)* Long parameters
- Streamline llm crate
- Use rig's `display_name()`
- Underscore unused variables
- *(config)* Align boolean fields with kdl specs
- *(session)* Session chat split
- *(tui)* Chat row estimation
- Update imports
- *(tools)* Split code
- *(session)* Busy state
- *(tui)* Replace mark dirty with message
- *(config)* Extract config to separate crate
- Add tui response / organize cli code

### 🎨 Styling

- Make background transparent
- Remove footer background
- Update padding
- Update footer position
- Update CLI help color
- Add decimal separator to token number
- Padding, version bar
- *(tui)* Add two spinners for status
- *(spinner)* Update frame rate
- *(tools)* Update display
- *(sidebar)* Update token format
- *(punctuation)* Update symbols
- *(tui)* Shell timeout
- *(tui)* Thinking block padding
- *(tui)* Thinking block arrow
- *(tui)* Update tool spinner animation
- *(tui)* Add wait spinner
- *(tui)* Context display for sidebar
- *(tui)* Change streaming text ;)
- *(tui)* Add padding to text block
- *(tools)* Remove output display for `list_dir` and `read_file`
- *(markdown)* Update tables and code blocks
- *(markdown)* Add markdown rendering for thinking blocks
- *(tools)* Update argument section for `edit_file`/`write_file`/`delete_file`
- *(tool)* Update `apply_patch` display
- *(todo)* Use full black circle for ongoing todo bullets
- *(config)* Update kdl indentation to 2 spaces
- *(diff)* Add edit emphasis

### 🧪 Testing

- Move integration test cases
- Fix test cases
- *(tui)* Frame rebuild
- Use `dir` directories

### ⚙️ Miscellaneous Tasks

- Update color scheme
- Update dependencies
- Update dependencies
- Update dependencies
- Update dependencies
- *(arrayref)* Update dependencies
- Update dependencies
- Split crate versions
- Update repo url
- Update dependencies
- Add lint and format
- Remove verbose output
- Disable on push run
- Update dependencies
- *(submodule)* Add selune
- Update hint text
- *(vendor)* Update selune
- *(cli)* Update help text
- Update ollama kind
- *(config)* Remove unused exclusion
- Update repo link
- *(prompt)* Update editor
- Remove unused test file
- Ignore lock_probe.rs
- Include lock_probe.rs
- *(uuid)* Use v7 solely
- Update description
- Remove vendor dependencies
- Add release action
- Remove submodule checkout
- Update fpm step for linux
- Put fpm install first
- Add nsis install
- Crosscompile windows build on linux
- Artifact expires in 1 day
- Fix windows build path

### 💼 Other

- Use MIT license
- Add release opts
- Update license claim
- *(tui)* Frame rate limit
- *(tui)* Cache committed-prefix render
- *(tui)* Extra polish
- *(agent)* Input token saving
- *(tools)* Subagent tool concurrency / LSP tools
- Add spinner
- Update
- Remove skill descriptions on tui
- *(tui)* Scheduler rework
- *(tui)* Dynamic chat scroll view
- *(tui)* Improve spinner responsiveness
- *(version)* Show commit short hash for debug build
- *(tools)* Allow multedit for todo tool
- *(tui)* Make tokio select scheduler fairer
- Use ThinLTO for improving compile time
- *(cargo)* Add descriptions
- *(nix)* Add nix flake config
- *(shuvarie)* Add local config
- *(shuvarie)* Update permissions
- *(shuvarie)* Update permission
- *(windows)* Add nsis installer script
- Add makefile
- Add end newline
