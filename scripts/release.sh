#!/usr/bin/env bash
# scripts/release.sh — cut a dracon-system release end-to-end.
#
# This command releases the dracon-system package from this repo: it updates
# Cargo.toml/CHANGELOG/release notes, the standalone Cargo.lock, crates.io,
# the dracon-system-vX.Y.Z tag, and its GitHub release.
#
# Hard rules baked into this script:
#   - The git tag is created only AFTER successful crates.io publish.
#     The tag is the contract that "this version is on crates.io".
#   - The repo working tree must be clean before starting. Run
#     this through `dracon-sync maintenance -- ...` to avoid daemon races.
#   - Every step is idempotent: re-running with the same version is a no-op
#     or a clear "already done" message.
#   - `--dry-run` runs every step without mutating remote state (no push,
#     no cargo publish for real, no gh release, no tag push). It still
#     modifies local release surfaces (Cargo.toml, Cargo.lock, CHANGELOG.md,
#     and the release-notes file) so the operator can inspect the diff;
#     `--abort` reverts exactly those local files. It CANNOT and does not undo
#     a real run's remote side effects (git tag, tag push, crates.io publish,
#     GitHub release) — a real run followed by `--abort` leaves those standing
#     and says so explicitly.
#
# Usage:
#   scripts/release.sh <version> [options]
#
#   <version>  e.g. 0.112.12  (NOT prefixed with 'v'; tag will be v<version>)
#
# Options:
#   --dry-run             Run the pipeline end-to-end without mutating remote
#                         state. Local release surfaces (Cargo.toml,
#                         Cargo.lock, CHANGELOG.md, release-notes file) ARE
#                         modified so the operator can inspect the diff. Use
#                         --abort to revert.
#   --abort               Revert any local modifications made by --dry-run
#                         (Cargo.toml + Cargo.lock + changelog +
#                         release-notes). Refuses to
#                         run if the working tree contains pre-existing
#                         modifications outside those release surfaces
#                         (CORRECTED 2026-08-11, audit MEDIUM: the guard is
#                         now real — the abort path used to run unchecked).
#   --remote <name>       Push to this git remote (default: auto-detect the
#                         github remote from remote.*.url).
#   --yes                 Skip the interactive "are you sure" prompt before
#                         push/publish/tag steps. Required for non-interactive
#                         runs.
#
# Examples:
#   scripts/release.sh 0.112.13 --dry-run        # safe preview
#   scripts/release.sh 0.112.13 --yes            # real cut
#   scripts/release.sh 0.112.13 --abort          # undo a dry-run
#
# Exit codes:
#   0  success
#   1  generic failure (inspect stdout/stderr)
#   2  precondition violation (dirty tree, missing credentials, etc.)
#   3  publish failed — tag NOT created, recovery steps in stderr

set -euo pipefail

# ----- paths ---------------------------------------------------------------
# FIXED 2026-09-01 (post-monorepo conversion 2026-08-22): the previous
# version of this script treated the utility directory as a standalone Git
# repository. The utility directories are now tracked inside the parent
# monorepo and are intentionally ignored for ordinary `git add`, so resolve
# both roots explicitly and force-stage only the release paths below.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
CRATE_REL="${CRATE_DIR#"$REPO_ROOT"/}"
# STANDALONE 2026-09-11 (nested-repo layout): the crate directory can
# BE the git top-level now. Release surfaces are then repo-root-relative.
if [[ "$CRATE_DIR" == "$REPO_ROOT" ]]; then CRATE_REL=""; fi
# Repo-root-relative prefix for release surfaces ("" in standalone mode).
RELPFX="${CRATE_REL:+$CRATE_REL/}"
cd "$REPO_ROOT"

CRATE_TOML="$CRATE_DIR/Cargo.toml"
CHANGELOG="$CRATE_DIR/CHANGELOG.md"
LOCKFILE="$REPO_ROOT/Cargo.lock"

# ----- defaults ------------------------------------------------------------
DRY_RUN=0
ABORT=0
# CHANGED 2026-10-03 (audit R4-M-10): empty = auto-detect the github
# remote from remote.*.url via scripts/resolve-github-remote.sh
# (ported from dracon-sync v0.113.11). The old REMOTE=origin default
# repeated the hardcoded-remote failure class on any non-origin
# naming; --remote remains as an explicit override.
REMOTE=""
ASSUME_YES=0
VERSION=""
CRATE_NAME="dracon-system"

# ----- argument parsing ----------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --abort)   ABORT=1; shift ;;
        --remote)  REMOTE="$2"; shift 2 ;;
        --yes)     ASSUME_YES=1; shift ;;
        -h|--help)
            sed -n '2,40p' "$0"
            exit 0
            ;;
        -*)
            echo "❌ unknown flag: $1" >&2
            exit 1
            ;;
        *)
            if [[ -z "$VERSION" ]]; then
                VERSION="$1"
            else
                echo "❌ unexpected positional arg: $1" >&2
                exit 1
            fi
            shift
            ;;
    esac
done

TAG="${CRATE_NAME}-v${VERSION}"
TOTAL_STEPS=8

# ----- colors (only on a tty) ---------------------------------------------
if [[ -t 1 ]]; then
    C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'
    # FIXED 2026-09-27 (audit rework round 3, F86): C_BOLD was assigned in
    # both branches and never read (shellcheck SC2034). Removed rather than
    # allow-listed so the colour set matches what the script actually uses.
    C_BLUE=$'\033[34m'; C_RESET=$'\033[0m'
else
    C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_RESET=""
fi

# ----- helpers -------------------------------------------------------------
log()    { printf '%s%s%s\n' "$C_BLUE" "$*" "$C_RESET"; }
ok()     { printf '%s%s%s\n' "$C_GREEN" "✓ $*" "$C_RESET"; }
warn()   { printf '%s%s%s\n' "$C_YELLOW" "⚠ $*" "$C_RESET"; }
die()    { printf '%s%s%s\n' "$C_RED" "✗ $*" "$C_RESET" >&2; exit 1; }
die_pre(){ printf '%s%s%s\n' "$C_RED" "✗ $*" "$C_RESET" >&2; exit 2; }
die_pub(){ printf '%s%s%s\n' "$C_RED" "✗ $*" "$C_RESET" >&2; exit 3; }

run() {
    # Print the command, then run it. Honors DRY_RUN.
    printf '   $ %s\n' "$*"
    if [[ $DRY_RUN -eq 1 ]]; then
        printf '   (skipped: --dry-run)\n'
        return 0
    fi
    "$@"
}

run_local() {
    # Local validation still runs during --dry-run.
    printf '   $ %s\n' "$*"
    "$@"
}

require_clean_tree() {
    if ! git diff --quiet HEAD 2>/dev/null || \
       [[ -n "$(git ls-files --others --exclude-standard)" ]] || \
       [[ -n "$(release_note_files)" ]]; then
        die_pre "working tree is dirty; commit or stash before releasing"
    fi
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die_pre "missing required command: $1"
}

require_credentials() {
    require_cmd gh; require_cmd cargo
    gh auth status >/dev/null 2>&1 \
        || die_pre "gh not authenticated; run 'gh auth login' first"
    [[ -f "$HOME/.cargo/credentials.toml" ]] \
        || die_pre "missing ~/.cargo/credentials.toml; run 'cargo login <token>' first"
}

is_release_surface() {
    local path=$1
    [[ "$path" == "${RELPFX}Cargo.toml" ||
       "$path" == "${RELPFX}CHANGELOG.md" ||
       "$path" == "Cargo.lock" ||
       "$path" == "${RELPFX}"release-notes-v*.md ]]
}

release_note_files() {
    # The parent .gitignore intentionally ignores the utility directory, so
    # include ignored-but-untracked release notes when handling --abort.
    git ls-files --others --ignored --exclude-standard -- \
        "${RELPFX}release-notes-v*.md" 2>/dev/null || true
}

confirm_remote_mutation() {
    [[ "$DRY_RUN" -eq 1 || "$ASSUME_YES" -eq 1 ]] && return 0
    if [[ ! -t 0 ]]; then
        die_pre "--yes is required for a non-interactive real release"
    fi
    local answer
    if ! read -r -p "Publish ${CRATE_NAME}@${VERSION}, tag ${TAG}, and push to ${REMOTE}? [y/N] " answer; then
        die_pre "release confirmation was not provided"
    fi
    case "${answer,,}" in
        y|yes) ;;
        *) die_pre "release cancelled" ;;
    esac
}

refresh_workspace_lock() {
    # A package version is part of the workspace lockfile. Cargo updates only
    # the affected local package entry here; unlike generate-lockfile this
    # does not discard the monorepo's intentionally pinned dependency graph.
    if ! cargo check -p "$CRATE_NAME" --quiet; then
        die_pre "failed to synchronize the workspace Cargo.lock for $CRATE_NAME@$VERSION"
    fi
    [[ -s "$LOCKFILE" ]] || die_pre "workspace Cargo.lock is missing after cargo check"
    ok "  Cargo.lock synchronized for $CRATE_NAME@$VERSION"
}

# ----- abort path ----------------------------------------------------------
if [[ $ABORT -eq 1 ]]; then
    log "Reverting local modifications from a previous --dry-run..."
    # Refuse to touch operator work outside this crate's release surfaces.
    other_modified=()
    while IFS= read -r f; do
        [[ -z "$f" ]] && continue
        if ! is_release_surface "$f"; then
            other_modified+=("$f")
        fi
    done < <(git diff --name-only HEAD 2>/dev/null || true)
    other_untracked=()
    while IFS= read -r f; do
        [[ -z "$f" ]] && continue
        if ! is_release_surface "$f"; then
            other_untracked+=("$f")
        fi
    done < <({ git ls-files --others --exclude-standard; release_note_files; } | sort -u)
    if [[ ${#other_modified[@]} -gt 0 || ${#other_untracked[@]} -gt 0 ]]; then
        die_pre "working tree dirty outside the release surfaces (${#other_modified[@]} modified, ${#other_untracked[@]} untracked); commit or stash first — --abort only reverts dry-run changes"
    fi
    abort_tracked=()
    while IFS= read -r f; do
        [[ -z "$f" ]] && continue
        if is_release_surface "$f"; then
            abort_tracked+=("$f")
        fi
    done < <(git diff --name-only HEAD 2>/dev/null || true)
    abort_untracked=()
    while IFS= read -r f; do
        [[ -z "$f" ]] && continue
        abort_untracked+=("$f")
    # STANDALONE 2026-09-11: release notes are tracked release artifacts, not
    # ignored files, so cover non-ignored untracked notes too (ignored-only
    # came free in the monorepo via the parent's utility-dir ignore rule).
    done < <({ release_note_files
        git ls-files --others --exclude-standard -- "${RELPFX}release-notes-v*.md" 2>/dev/null || true
    } | sort -u)
    if [[ ${#abort_tracked[@]} -gt 0 || ${#abort_untracked[@]} -gt 0 ]]; then
        set +e
        if [[ ${#abort_tracked[@]} -gt 0 ]]; then
            git restore --source=HEAD --staged --worktree -- "${abort_tracked[@]}" 2>/dev/null
        fi
        if [[ ${#abort_untracked[@]} -gt 0 ]]; then
            rm -f -- "${abort_untracked[@]}" 2>/dev/null
        fi
        set -e
        ok "local modifications reverted (${#abort_tracked[@]} tracked, ${#abort_untracked[@]} untracked)"
    else
        ok "no local modifications to revert"
    fi
    exit 0
fi

# ----- preconditions -------------------------------------------------------
[[ -n "$VERSION" ]] || die_pre "missing <version> argument; see --help"

require_credentials
require_clean_tree

# Resolve the github push remote (2026-10-03, audit R4-M-10, ported
# from dracon-sync v0.113.11): derived by URL, not hardcoded — the old
# REMOTE=origin default failed on any non-origin remote naming. Loud
# failure when no github remote exists; an explicit --remote override
# is validated instead.
if [[ -n "$REMOTE" ]]; then
    git config --get "remote.${REMOTE}.url" >/dev/null 2>&1 \
        || die_pre "remote '$REMOTE' does not exist (git config remote.$REMOTE.url)"
    ok "push remote: $REMOTE (explicit --remote override)"
else
    REMOTE="$("$SCRIPT_DIR/resolve-github-remote.sh" "$REPO_ROOT")" \
        || die_pre "could not resolve a github remote (see above)"
    ok "push remote: $REMOTE (auto-detected from remote.*.url)"
fi

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    die_pre "version '$VERSION' is not semver (expected e.g. 0.112.12)"
fi

# The manifest version that matters is the one in [package], not the first
# `^version =` line in the file: a future [workspace.package] block above it
# would otherwise be the line that gets bumped (audit 2026-10-01).
crate_manifest_version() {
    awk -F'"' '
        /^\[/ { in_package = ($0 == "[package]"); next }
        in_package && /^version[[:space:]]*=/ { print $2; exit }
    ' "$CRATE_TOML"
}
CURRENT_VERSION="$(crate_manifest_version)"
[[ -n "$CURRENT_VERSION" ]] || die_pre "no [package] version found in $CRATE_TOML"

# Monotonicity (DECIDED 2026-10-01): a version that is not newer than the
# current one rewrites the manifest, closes the CHANGELOG under a misleading
# header, and only fails later at the registry. Refuse it here, before
# anything has been touched.
if [[ "$(printf '%s\n%s\n' "$CURRENT_VERSION" "$VERSION" | sort -V | head -1)" != "$CURRENT_VERSION" ]]; then
    die_pre "version '$VERSION' is not newer than the current $CURRENT_VERSION in ${RELPFX}Cargo.toml — refusing to release a downgrade or a re-publish"
fi

# Resolve the GitHub path once: both the generated release notes and the final
# summary need it, and the notes are written long before the old late lookup.
# Only a real github.com remote is trusted; anything else (a local fixture
# path, a self-hosted remote) falls back to this repo's documented home, so a
# mangled path can never end up in a published compare link.
REMOTE_URL="$(git config --get "remote.${REMOTE}.url" 2>/dev/null || true)"
if [[ "$REMOTE_URL" =~ github\.com[:/]+([^/]+/[^/]+?)(\.git)?$ ]]; then
    # FIXED 2026-10-03 (audit R4-M-03 follow-up): bash ERE has no lazy
    # quantifiers, so `+?` matches greedily — group 1 kept a `.git`
    # suffix (group 2 matched empty) and both github.com and
    # raw.githubusercontent.com 404 on the suffixed form. Strip it.
    GH_PATH="${BASH_REMATCH[1]%.git}"
else
    GH_PATH="DraconDev/dracon-system-disk-process-guard-doctor"
    [[ "$REMOTE_URL" == *"github.com"* ]] || log "  origin '$REMOTE_URL' is not a github.com remote; using the documented repo path for links"
fi

# ----- step 1: test discipline gates (AGENTS.md) -------------------------
log "step 1/${TOTAL_STEPS}: test discipline gates (AGENTS.md)"
# Run the repository's mandatory gates before any release-surface mutation.
# They also run for --dry-run before any release-surface mutation; only
# ignored target/ artifacts are produced by the gates themselves.
require_cmd cargo-deny
run_gate() {
    printf '   $ %s\n' "$*"
    "$@"
}
# ADDED 2026-09-28 (audit decision D5 follow-up): `cargo deny check` does
# NOT fail when it cannot find a config. cargo-deny 0.19.9 logs
#   [WARN] unable to find a config path, falling back to default config
# and then silently applies its BUILT-IN policy, which is neither this
# repo's nor the workspace's. Reproduced in a bare clone with no
# `deny.toml` in scope: `advisories FAILED, bans ok, licenses FAILED,
# sources ok` (exit 5), because the default allow-list rejects dual
# expressions such as `MIT OR Apache-2.0` that both our configs permit.
# A release gate that can quietly enforce a third policy is worse than no
# gate, so the fallback is now an explicit, fatal error.
run_deny_gate() {
    printf '   $ cargo deny check\n'
    local out rc=0
    out="$(cargo deny check 2>&1)" || rc=$?
    printf '%s\n' "$out"
    if grep -q "falling back to default config" <<<"$out"; then
        die_pre "no cargo-deny config in scope: cargo-deny fell back to its BUILT-IN default policy, which is not this repository's policy. Run the release from the parent workspace (where deny.toml resolves upward), or pass --config explicitly."
    fi
    [ "$rc" -eq 0 ] || die "cargo deny check failed (exit $rc)"
}
run_gate cargo test --workspace --locked
run_gate cargo build --release --locked
run_deny_gate
run_gate cargo clippy --workspace --locked -- -D warnings
ok "  all gates passed"

# ----- step 2: bump Cargo.toml version ------------------------------------
log "step 2/${TOTAL_STEPS}: bumping ${RELPFX}Cargo.toml to ${VERSION}"
current="$CURRENT_VERSION"
if [[ "$current" == "$VERSION" ]]; then
    ok "  $CRATE_TOML already at $VERSION"
else
    # Rewrite only the [package] version line, and fail loudly rather than
    # silently leaving a stale version behind.
    toml_tmp="$(mktemp "${CRATE_TOML}.XXXXXX")"
    if ! awk -v v="$VERSION" '
            # print the section header too — dropping it would leave a
            # manifest with no [package] at all, which no longer parses.
            /^\[/ { in_package = ($0 == "[package]"); print; next }
            in_package && /^version[[:space:]]*=/ && !done { print "version = \"" v "\""; done = 1; next }
            { print }
            END { if (!done) exit 1 }
        ' "$CRATE_TOML" > "$toml_tmp"; then
        rm -f "$toml_tmp"
        die "could not rewrite the [package] version in $CRATE_TOML"
    fi
    mv "$toml_tmp" "$CRATE_TOML"
    ok "  $CRATE_TOML: $current → $VERSION"
fi

refresh_workspace_lock

# ----- step 3: close CHANGELOG [Unreleased] -------------------------------
log "step 3/${TOTAL_STEPS}: closing ${RELPFX}CHANGELOG.md [Unreleased] → [${VERSION}]"
DATE=$(date -u +%Y-%m-%d)
# FIXED 2026-08-11 (audit HIGH): extracted the inline closer into the
# tested idempotent helper. Re-running after a partial release now leaves an
# existing version header byte-identical instead of duplicating it. A dry-run
# deliberately writes the local release surface so --abort has real work.
# Refuse to close an empty [Unreleased]: the closer would write a bare
# version header with no body, producing a release whose notes say nothing
# (audit 2026-10-01). Checked before any mutation.
# The idempotent re-run of an already-released version is NOT blocked: a
# closed "[$VERSION]" header means the notes were shipped by the first run,
# and refusing there would break the "re-running with the same version is a
# no-op" hard rule above.
if ! awk -v want="$VERSION" '
    $0 == "## [" want "]" || $0 ~ ("^## \\[" want "\\]") { closed = 1 }
    /^## \[Unreleased\]/ { seen = 1; next }
    /^## \[/ { seen = 0 }
    seen && /^[^[:space:]#]/ { found = 1 }
    END { exit (closed || found) ? 0 : 1 }
' "$CHANGELOG"; then
    die_pre "$CHANGELOG has no content under [Unreleased] — write the release notes first, or there is nothing to ship"
fi
python3 "$SCRIPT_DIR/close-changelog.py" "$CHANGELOG" "$VERSION" "$DATE"
ok "  $CHANGELOG: [Unreleased] closed as [${VERSION}] - ${DATE} (or already closed)"

# ----- step 4: create release-notes file ----------------------------------
log "step 4/${TOTAL_STEPS}: creating ${RELPFX}release-notes-v${VERSION}.md"
NOTES_REL="${RELPFX}release-notes-v${VERSION}.md"
NOTES="$REPO_ROOT/$NOTES_REL"
if [[ -f "$NOTES" ]]; then
    ok "  $NOTES_REL already exists"
else
    cat > "$NOTES" <<EOF
# dracon-system v${VERSION} (${DATE})

Invisible git sync daemon for deterministic AI-assisted development.

## What's Changed

- Bump version to ${VERSION}
- (See CHANGELOG.md for the full list of changes in this release)

## Install

\`\`\`bash
# --root "\$HOME/.local" puts the binary where the shipped unit's ExecStart
# (%h/.local/bin/dracon-system) expects it. A bare \`cargo install\` lands it in
# ~/.cargo/bin instead, and the service then dies with 203/EXEC.
cargo install dracon-system --version ${VERSION} --root "\$HOME/.local"
\`\`\`

## Docker / systemd

\`\`\`bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/${GH_PATH}/main/dracon-system-guard.service \\
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
\`\`\`

**Full Changelog**: https://github.com/${GH_PATH}/compare/$(git describe --tags --abbrev=0 2>/dev/null || echo "dracon-system-v0.0.0")...${TAG}
EOF
    ok "  $NOTES_REL created"
fi

# ----- step 5: cargo publish --dry-run (sanity) ---------------------------
log "step 5/${TOTAL_STEPS}: cargo publish --dry-run (sanity check)"
run_local cargo publish -p "$CRATE_NAME" --dry-run --allow-dirty

# ----- step 6: cargo publish for real -------------------------------------
log "step 6/${TOTAL_STEPS}: cargo publish -p $CRATE_NAME"
# Idempotent re-run path: an already-published version is success when a
# previous run failed after the crates.io upload.
confirm_remote_mutation
if [[ $DRY_RUN -eq 1 ]]; then
    run cargo publish -p "$CRATE_NAME" --allow-dirty
else
    printf '   $ cargo publish -p %s --allow-dirty\n' "$CRATE_NAME"
    if ! publish_out="$(cargo publish -p "$CRATE_NAME" --allow-dirty 2>&1)"; then
        if grep -qiE "already exists on crates.io index|already published" <<<"$publish_out"; then
            ok "  $CRATE_NAME@$VERSION already published; continuing"
        else
            printf '%s\n' "$publish_out" >&2
            die_pub "cargo publish failed — tag NOT created"
        fi
    fi
fi

# ----- step 7: fixture check on the published artifact -------------------
log "step 7/${TOTAL_STEPS}: fixture check on packaged artifact"
# Installing from target/package reproduces the dependency resolution of a
# crates.io install. A broken packaged binary must not be tagged or released.
PKG_ROOT="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["workspace_root"])' \
    2>/dev/null || echo "$REPO_ROOT")"
PKG_DIR="$PKG_ROOT/target/package/${CRATE_NAME}-${VERSION}"
if [[ -d "$PKG_DIR" ]]; then
    FIXTURE_ROOT="$PKG_ROOT/target/fixture-bin"
    if [[ $DRY_RUN -eq 1 ]]; then
        printf '   $ cargo install --path %s --root %s --force  (skipped: --dry-run)\n' "$PKG_DIR" "$FIXTURE_ROOT"
    else
        run cargo install --path "$PKG_DIR" --root "$FIXTURE_ROOT" --force
        if ! "$SCRIPT_DIR/verify-install.sh" "$FIXTURE_ROOT/bin/dracon-system" "$VERSION"; then
            die_pub "fixture check FAILED on the packaged artifact — release is broken, do NOT tag"
        fi
    fi
elif [[ $DRY_RUN -eq 1 ]]; then
    warn "  packaged crate dir not present (publish was skipped in --dry-run); fixture check skipped"
else
    die_pub "packaged crate dir $PKG_DIR missing — cannot run fixture check (publish must have failed)"
fi

# ----- step 8: commit, tag, push, gh release ------------------------------
log "step 8/${TOTAL_STEPS}: commit, tag, push, gh release"
# Force staging is scoped to the exact release surfaces (also covers
# ignored release-note paths) and never uses `git add .`.
run git add -f -- "${RELPFX}Cargo.toml" "Cargo.lock" "${RELPFX}CHANGELOG.md" "$NOTES_REL"
# Idempotent re-run path: skip already-completed commit, tag, and GitHub
# release operations when a previous run failed later in the pipeline.
if [[ $DRY_RUN -eq 1 ]]; then
    run git commit -m "release: v${VERSION}"
    run git tag "$TAG"
else
    if git diff --cached --quiet; then
        ok "  nothing to commit (release commit already exists)"
    else
        printf '   $ git commit -m release: v%s\n' "$VERSION"
        git commit -m "release: v${VERSION}"
    fi
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        ok "  tag $TAG already exists"
    else
        printf '   $ git tag %s\n' "$TAG"
        git tag "$TAG"
    fi
fi
run git push "$REMOTE" main "$TAG"

if [[ $DRY_RUN -eq 1 ]]; then
    run gh release create "$TAG" \
        --target main \
        --title "${CRATE_NAME} v${VERSION}" \
        --notes-file "$NOTES"
else
    if gh release view "$TAG" >/dev/null 2>&1; then
        ok "  github release $TAG already exists"
    else
        printf '   $ gh release create %s\n' "$TAG"
        gh release create "$TAG" \
            --target main \
            --title "${CRATE_NAME} v${VERSION}" \
            --notes-file "$NOTES"
    fi
fi

# Mirror remotes receive main from the daemon, but tags are operator-pushed.
# Print exact commands so a release cannot silently leave mirrors tagless.
mirror_remotes=()
while IFS= read -r mline; do
    mkey="${mline%% *}"
    mname="${mkey#remote.}"; mname="${mname%.url}"
    [[ "$mname" == "$REMOTE" ]] || mirror_remotes+=("$mname")
done < <(git config --get-regexp '^remote\..*\.url$' || true)
if [[ ${#mirror_remotes[@]} -gt 0 ]]; then
    warn ""
    warn "mirror remotes get main from the daemon, but tags are operator-pushed:"
    for m in "${mirror_remotes[@]}"; do
        warn "    git push $m $TAG"
    done
fi

ok ""
ok "════════════════════════════════════════════"
ok "✓ dracon-system v${VERSION} released"
ok "  crates.io:  https://crates.io/crates/dracon-system"
ok "  github:     https://github.com/${GH_PATH}/releases/tag/${TAG}"
ok "════════════════════════════════════════════"

warn ""
warn "after 'cargo install dracon-system --version ${VERSION}', run the fixture check:"
warn "    ${RELPFX}scripts/verify-install.sh"

# A released unit that is never copied to ~/.config/systemd/user leaves the live
# service on the old unit, silently: F94 (2026-09-27) and ExecReload (2026-09-29)
# both shipped and only surfaced later as a command that would not work. Nothing
# in a release can own host deployment state, so surface it here instead —
# advisory, never fatal, because a stale local unit must not block a release.
if [[ -x "${RELPFX}scripts/check-unit-deployment.sh" ]]; then
    warn ""
    if ! "${RELPFX}scripts/check-unit-deployment.sh"; then
        warn "the deployed systemd unit is out of date with this release (see above)."
        warn "    install -m 644 ${RELPFX}dracon-system-guard.service ~/.config/systemd/user/dracon-system-guard.service && systemctl --user daemon-reload"
    fi
fi

if [[ $DRY_RUN -eq 1 ]]; then
    echo ""
    warn "This was a --dry-run. Local release surfaces were modified but no remote state was changed."
    warn "Run '${RELPFX}scripts/release.sh --abort' to revert, or '${RELPFX}scripts/release.sh ${VERSION} --yes' to execute for real."
fi
