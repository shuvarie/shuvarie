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

# Regenerate CHANGELOG.md with git-cliff (multi-branch aware, deduped)
changelog:
    #!/usr/bin/env sh
    # Unlike a plain `git cliff <tag>..HEAD`, the walk is not bound to the
    # current branch: it unions the trunk (HEAD) with every branch tip that
    # carries a release tag HEAD cannot reach (e.g. v0.2.3 living only on
    # rel/0.2), so side-branch tags cut their own sections and their line's
    # hotfix commits are attributed to them.
    #
    # Commits cherry-picked between the trunk and the release branches are
    # deduplicated: for each duplicated change, only the copy that shipped in
    # the oldest release section survives, so [unreleased] and later sections
    # do not repeat what a rel/* release already delivered. Conflict-adjusted
    # cherry picks get different patch IDs (git cherry can't catch them), so
    # matching is done on the normalized subject. Prefer `git cherry-pick -x`
    # when copying onto a release branch so the audit trail keeps the link.
    #
    # Mechanics: git-cliff walks a single libgit2 revspec, hence the throwaway
    # union tip built via `git commit-tree` (plumbing: no worktree, index or
    # refs are touched; the dangling object is gc'ed later). Release sections
    # mirror git-cliff's own graph-reachability assignment (2.14+, #1601): a
    # commit belongs to the earliest tag whose commit can reach it. The union
    # tip itself and the deduplicated copies are suppressed with
    # --skip-commit.
    #
    # See cliff.toml for commit grouping and the render template.
    set -eu

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    head=$(git rev-parse HEAD)
    if [ -z "$(git tag)" ]; then
        echo "error: no tags found; nothing to generate" >&2
        exit 1
    fi

    # tags.tsv: tagged-commit<TAB>tag-name, oldest first. The oldest tag
    # bounds the walk (everything at or below it is published history).
    for t in $(git tag --sort=creatordate); do
        printf '%s\t%s\n' "$(git rev-parse "${t}^{}")" "$t"
    done > "$tmp/tags.tsv"
    base=$(awk -F'\t' 'NR==1 { print $1; exit }' "$tmp/tags.tsv")

    # Unreached: tags the trunk line cannot reach (the rel/* hotfix releases).
    unreached=""
    while IFS=$(printf '\t') read -r c t; do
        case " $unreached " in *" $c "*) continue ;; esac
        git merge-base --is-ancestor "$c" "$head" || unreached="$unreached $c"
    done < "$tmp/tags.tsv"

    # extra_tips: branch tips carrying at least one unreached tag. They join
    # the walk so those tags become release boundaries alongside HEAD's.
    extra_tips=""
    for ref in $(git branch -a --format='%(refname:short)'); do
        case "$ref" in */HEAD) continue ;; esac
        tip=$(git rev-parse --verify "$ref" 2>/dev/null) || continue
        [ "$tip" != "$head" ] || continue
        case " $extra_tips " in *" $tip "*) continue ;; esac
        for u in $unreached; do
            if git merge-base --is-ancestor "$u" "$tip"; then
                extra_tips="$extra_tips $tip"
                break
            fi
        done
    done

    # Synthetic union tip so ONE libgit2 revspec can walk the trunk and all
    # release lines at once (git-cliff range = oldest-tag..<union-tip>).
    parents="-p $head"
    for t in $extra_tips; do
        parents="$parents -p $t"
    done
    union=$(git commit-tree "$(git rev-parse "${head}^{tree}")" \
        $parents -m "git-cliff: temporary branch-union tip for the changelog walk")

    # walked.tsv: every commit of the union walk.
    git rev-list "$head" ${extra_tips} --not "$base" | sort > "$tmp/walked"

    # sections.tsv: walked-commit<TAB>owning tag, where the owner is the
    # earliest tag (chronologically) whose commit can reach it; commits no
    # tag reaches stay unreleased. Mirrors git-cliff's own grouping so the
    # dedup below can rank copies by release.
    : > "$tmp/sections"
    hide=""
    while IFS=$(printf '\t') read -r c t; do
        git rev-list "$c" ${hide} | sort > "$tmp/tagrev"
        comm -12 "$tmp/walked" "$tmp/tagrev" |
            awk -v tag="$t" '{ print $1 "\t" tag }' >> "$tmp/sections"
        hide="$hide ^$c"
    done < "$tmp/tags.tsv"
    awk -F'\t' '
        NR==FNR { rank[$2] = FNR; next }
        $2 in rank { print $1 "\t" $2 "\t" rank[$2] }
    ' "$tmp/tags.tsv" "$tmp/sections" > "$tmp/ranked"

    # members.tsv: commit<TAB>normalized-subject<TAB>side (trunk or branch).
    : > "$tmp/members"
    while IFS= read -r h; do
        if git merge-base --is-ancestor "$h" "$head"; then side=trunk; else side=branch; fi
        subj=$(git log -1 --format='%s' "$h" |
            sed -E 's/^[a-z]+\(([^)]*)\): /:/; s/^[a-z]+: /:/')
        printf '%s\t%s\t%s\n' "$h" "$subj" "$side" >> "$tmp/members"
    done < "$tmp/walked"

    # Dedup: in subject groups that span the trunk and a release branch,
    # keep only the copy owned by the oldest release section; the rest are
    # suppressed from the changelog.
    awk -F'\t' '
        NR==FNR { rk[$1] = $3; next }
        {
            k = $2; h = $1; s = $3
            r = (h in rk) ? rk[h] : 999999
            n[k]++; m[k, n[k]] = h; q[k, n[k]] = r
            if (s == "trunk") tside[k] = 1; else bside[k] = 1
        }
        END {
            for (k in n) {
                if (n[k] < 2 || !(k in tside) || !(k in bside)) continue
                best = 0; br = 999999
                for (i = 1; i <= n[k]; i++) {
                    if (q[k, i] < br) { br = q[k, i]; best = i }
                }
                for (i = 1; i <= n[k]; i++) {
                    if (i != best) print m[k, i]
                }
            }
        }
    ' "$tmp/ranked" "$tmp/members" > "$tmp/skip"

    skipargs="--skip-commit $union"
    ndups=0
    while IFS= read -r s; do
        skipargs="$skipargs --skip-commit $s"
        ndups=$((ndups + 1))
        echo "dedup: skipping cherry-picked duplicate $s $(git log -1 --format='%s' "$s")"
    done < "$tmp/skip"

    git cliff "${base}..${union}" $skipargs -o ./CHANGELOG.md

# Regenerate CHANGELOG.md scoped to the current branch only (--use-branch-tags)
changelog-branch:
    git cliff --use-branch-tags v0.1.1..HEAD -o ./CHANGELOG.md
