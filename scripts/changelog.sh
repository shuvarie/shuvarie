#!/usr/bin/env sh
# changelog.sh - branch-aware CHANGELOG.md generation with cherry-pick dedup.
#
# Unlike a plain `git cliff <tag>..HEAD`, the walk is not bound to the current
# branch: it unions the trunk (HEAD) with every branch tip that carries a
# release tag HEAD cannot reach (e.g. v0.2.3 living only on rel/0.2), so
# side-branch tags cut their own sections and their line's hotfix commits are
# attributed to them.
#
# Commits cherry-picked between the trunk and the release branches are
# deduplicated: for each duplicated change, only the copy that shipped in the
# oldest release section survives, so [unreleased] and later sections do not
# repeat what a rel/* release already delivered. Conflict-adjusted cherry
# picks get different patch IDs (git cherry can't catch them), so matching is
# done on the normalized subject. Prefer `git cherry-pick -x` when copying
# onto a release branch so the audit trail keeps the link.
#
# Mechanics: git-cliff walks a single libgit2 revspec, hence the throwaway
# union tip built via `git commit-tree` (plumbing: no worktree, index or refs
# are touched; the dangling object is gc'ed later). Release sections mirror
# git-cliff's own graph-reachability assignment (2.14+, #1601): a commit
# belongs to the earliest tag whose commit can reach it. The union tip itself
# and the deduplicated copies are suppressed with --skip-commit.
#
# Modes (the pipeline is shared by the justfile and .github/workflows/release.yml):
#
#   changelog.sh
#       Render the full ./CHANGELOG.md.
#
#   changelog.sh --release <tag>
#       Print the tag's section as a release note: the full pipeline (union
#       walk, dedup, render, slice) in one shot. Requires git-cliff in PATH.
#       From any checkout - walk/dedup/slice decisions are checkout-independent
#       for tagged sections.
#
#   changelog.sh --prepare <tag>
#       CI part 1: compute the branch-union walk range and the deduplicated
#       copies to suppress, printing the report, and - when $GITHUB_OUTPUT is
#       set (GitHub Actions) - exporting `version` and `args` (range +
#       --skip-commit flags) for a later `git cliff` invocation. Needs no
#       git-cliff binary: it only orchestrates git plumbing.
#
#   changelog.sh --slice <version> [file]
#       CI part 2: extract the `## [<version>] - <date>` section from a
#       rendered changelog file (defaults to standard input) as the release
#       note body. Needs no git access.
#
# See cliff.toml for commit grouping and the render template.

set -eu

usage() {
    echo "usage: changelog.sh [--release <tag> | --prepare <tag> | --slice <version> [file]]" >&2
}

die() {
    echo "error: $1" >&2
    exit 1
}

# Resolve a tag passed with or without the leading "v" into the git tag name
# ($rawtag) and the rendered section name ($version - cliff.toml's template
# strips the leading "v" from release headings).
resolve_tag() {
    case "$1" in
        v*)
            rawtag=$1
            version=${1#v}
            ;;
        *)
            version=$1
            if git rev-parse --verify -q "refs/tags/$1" >/dev/null; then
                rawtag=$1
            elif git rev-parse --verify -q "refs/tags/v$1" >/dev/null; then
                rawtag="v$1"
            else
                die "tag '$1' does not exist"
            fi
            ;;
    esac
}

# Compute the union walk, section ownership and the deduplicated copies:
# exports tmp, base, union, args_out (and the skip list in "$tmp/skip").
# The tag-related modes resolve/validate their tag with resolve_tag before
# calling; the no-argument full-render mode needs none: it renders every
# section from the current HEAD.
compute() {
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    head=$(git rev-parse HEAD)
    if [ -z "$(git tag)" ]; then
        die "no tags found; nothing to generate"
    fi

    # tags.tsv: tagged-commit<TAB>tag-name, oldest first. The oldest tag
    # bounds the walk (everything at or below it is published history).
    for t in $(git tag --sort=creatordate); do
        printf '%s\t%s\n' "$(git rev-parse "${t}^{}")" "$t"
    done > "$tmp/tags.tsv"
    base=$(awk -F'\t' 'NR==1 { print $1; exit }' "$tmp/tags.tsv")

    # Unreached: tags the trunk line cannot reach (the rel/* hotfix releases).
    unreached=""
    while IFS="$(printf '\t')" read -r c t; do
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
    while IFS="$(printf '\t')" read -r c t; do
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

    args_out="${base}..${union} --skip-commit $union"
    while IFS= read -r s; do
        args_out="$args_out --skip-commit $s"
        echo "dedup: skipping cherry-picked duplicate $s $(git log -1 --format='%s' "$s")" >&2
    done < "$tmp/skip"
}

# Render the changelog over the $args_out computed above (range + skip
# flags) into the given output path. Requires a git-cliff binary in PATH.
#
# $args_out is deliberately expanded unquoted (POSIX sh has no arrays): it
# carries the walk range plus one "--skip-commit <sha>" flag per suppressed
# copy, all whitespace-safe single tokens the action word-splits from, and
# this same string is what release.yml passes through the git-cliff action.
render_to() {
    command -v git-cliff >/dev/null 2>&1 ||
        die "git-cliff is not installed (https://git-cliff.org/docs/installation)"
    git cliff $args_out -o "$1"
}

# Extract the `## [<version>] - <date>` section from a rendered changelog
# file ("-" for standard input) and print it. The section is byte-identical
# to the CHANGELOG.md entry for that tag (the changelog header and sections
# outside it are dropped); trailing blank lines are trimmed by trimming the
# command substitution's trailing newlines.
slice() {
    notes=$(awk -v want="$1" '
        body == 0 && /^## \[/ {
            h = $0
            sub(/^## \[/, "", h)
            sub(/\].*$/, "", h)
            if (h == want) { body = 1; print; next }
        }
        body == 1 && /^## \[/ { exit }
        body == 1 { print }
        END { if (!body) exit 1 }
    ' "$2") ||
        die "no section for '$1' found in the rendered changelog; is the tag missing or ignored by git-cliff?"

    case "$notes" in
        "## [$1]"*) ;;
        *) die "unexpected section content for '$1'" ;;
    esac
    printf '%s\n' "$notes"
}

# Dispatch: informational output (dedup report, walk description) goes to
# stderr in every mode so that artifacts rendered to stdout stay clean.
case "${1:-}" in
    --help | -h)
        grep -E '^#( |$)' "$0" | sed 's/^# \{0,1\}//' >&2
        exit 0
        ;;
    "")
        compute
        render_to ./CHANGELOG.md
        ;;
    --release)
        [ "$#" -eq 2 ] || { usage; exit 2; }
        resolve_tag "$2"
        compute
        render_to "$tmp/render.md"
        slice "$version" "$tmp/render.md"
        ;;
    --prepare)
        [ "$#" -eq 2 ] || { usage; exit 2; }
        resolve_tag "$2"
        compute
        echo "branch-union walk: ${base}..${union}" >&2
        if [ -n "${GITHUB_OUTPUT:-}" ]; then
            {
                printf 'version=%s\n' "$version"
                printf 'args=%s\n' "$args_out"
            } >> "$GITHUB_OUTPUT"
        fi
        ;;
    --slice)
        [ "$#" -eq 2 ] || [ "$#" -eq 3 ] || { usage; exit 2; }
        # The section heading carries the rendered version (v-stripped);
        # accept the tag form too.
        slice "${2#v}" "${3:--}"
        ;;
    *)
        usage
        exit 2
        ;;
esac