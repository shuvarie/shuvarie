# justfile - common development tasks for shuvarie.
#
# Native targets (any host):
#   just build          Build the workspace (debug)
#   just run            Build and run the TUI
#   just test           Run the test suite
#   just clippy         Lint the workspace
#   just fmt            Check formatting
#   just clean          Remove build artifacts
#
# Windows targets (release, see nsis/README.md):
#   just build-win      Build the Windows release binary
#   just win-installer  Build the Windows installer (requires makensis)
#
# The Windows target defaults to x86_64-pc-windows-msvc on a Windows host and
# x86_64-pc-windows-gnu elsewhere. Override it with `just WIN_TARGET=<triple>`.
# Cross-compiling from a POSIX host additionally needs the target's standard
# library: rustup target add <triple>

# Workspace version, read from Cargo.toml at parse time.
version_raw := `sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml`
version := if version_raw == "" {
    error("failed to read the workspace version from Cargo.toml")
} else {
    version_raw
}

# Windows release target and the matching makensis definition-switch prefix.
# Override the target with `just WIN_TARGET=<triple> build-win`.
WIN_TARGET := if os() == "windows" {
    env_var_or_default("WIN_TARGET", "x86_64-pc-windows-msvc")
} else {
    env_var_or_default("WIN_TARGET", "x86_64-pc-windows-gnu")
}
makensis_d := if os() == "windows" { "/D" } else { "-D" }
win_exe := "target/" + WIN_TARGET + "/release/shuvarie.exe"

# Cross-compiling from a POSIX host needs the target standard library; the
# msvc toolchain on a Windows host always ships it, so only guard there.
std_check := if os() == "windows" { "" } else {
    "if ! [ -d \"$(rustc --print target-libdir --target " + WIN_TARGET + ")\" ]; then " +
        "echo \"error: no Rust standard library for " + WIN_TARGET + ".\" >&2; " +
        "echo \"       install it with: rustup target add " + WIN_TARGET + "\" >&2; " +
        "exit 1; fi"
}

# Build the workspace (debug)
default: build

# Build the workspace (debug)
build:
    cargo build

# Build and run the TUI
run:
    cargo run

# Run the test suite
test:
    cargo test

# Lint the workspace
clippy:
    cargo clippy --all-targets

# Check formatting
fmt:
    cargo fmt --check

# Remove build artifacts
clean:
    cargo clean

# Build the Windows release binary
build-win:
    {{ if os() == "windows" { "# target std libs ship with the host toolchain" } else { std_check } }}
    cargo build --release --target {{ WIN_TARGET }} --bin shuvarie

# Build the Windows installer (requires makensis)
win-installer: build-win
    makensis {{ makensis_d }}VERSION="{{ version }}" \
        {{ makensis_d }}OUT_FILE="{{ justfile_directory() }}/nsis/shuvarie-setup-{{ version }}.exe" \
        {{ makensis_d }}BINARY="{{ justfile_directory() }}/{{ win_exe }}" \
        nsis/installer.nsi
    @echo "Windows installer written to nsis/shuvarie-setup-{{ version }}.exe"

# Regenerate CHANGELOG.md with git-cliff
changelog:
    git cliff v0.1.1..HEAD -o ./CHANGELOG.md
