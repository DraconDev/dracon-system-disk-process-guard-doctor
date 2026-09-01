#!/usr/bin/env bash
# scripts/release.sh — cut a dracon-system release end-to-end.
#
# This command releases the dracon-system package from the dracon-utilities
# monorepo: it updates the utility's Cargo.toml/CHANGELOG/release notes,
# the monorepo lockfile, crates.io, the monorepo tag, and its GitHub release.
#
# Hard rules baked into this script:
#   - The git tag is created only AFTER successful crates.io publish.
#     The tag is the contract that "this version is on crates.io".
#   - The parent monorepo working tree must be clean before starting. Run
#     this through `dracon-sync maintenance -- ...` to avoid daemon races.
#   - Every step is idempotent: re-running with the same version is a no-op
#     or a clear "already done" message.
#   - `--dry-run` runs every step without mutating remote state (no push,
#     no cargo publish for real, no gh release, no tag push). It still
#     modifies local release surfaces (Cargo.toml, Cargo.lock, CHANGELOG.md,
#     and the release-notes file) so the operator can inspect the diff;
#     `--abort` reverts them.
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
#   --remote <name>       Push to this git remote (default: origin).
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
cd "$REPO_ROOT"

CRATE_TOML="$CRATE_DIR/Cargo.toml"
CHANGELOG="$CRATE_DIR/CHANGELOG.md"
LOCKFILE="$REPO_ROOT/Cargo.lock"

# ----- defaults ------------------------------------------------------------
DRY_RUN=0
ABORT=0
# CHANGED 2026-08-10 (v0.112.35): default was `github`, but this repo
# names its GitHub remote `origin` — release.sh 0.112.35 step 6 failed
# with "fatal: 'github' does not appear to be a git repository". The
# push was completed manually to origin/codeberg/gitlab.
REMOTE=origin
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

TAG="v${VERSION}"
TOTAL_STEPS=8

# ----- colors (only on a tty) ---------------------------------------------
if [[ -t 1 ]]; then
    C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'
    C_BLUE=$'\033[34m'; C_BOLD=$'\033[1m'; C_RESET=$'\033[0m'
else
    C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_BOLD=""; C_RESET=""
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
    [[ "$path" == "$CRATE_REL/Cargo.toml" ||
       "$path" == "$CRATE_REL/CHANGELOG.md" ||
       "$path" == "Cargo.lock" ||
       "$path" == "$CRATE_REL"/release-notes-v*.md ]]
}

release_note_files() {
    # The parent .gitignore intentionally ignores the utility directory, so
    # include ignored-but-untracked release notes when handling --abort.
    git ls-files --others --ignored --exclude-standard -- \
        "$CRATE_REL/release-notes-v*.md" 2>/dev/null || true
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
    done < <(release_note_files)
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

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    die_pre "version '$VERSION' is not semver (expected e.g. 0.112.12)"
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
run_gate cargo test --workspace --locked
run_gate cargo build --release --locked
run_gate cargo deny check
run_gate cargo clippy --workspace --locked -- -D warnings
ok "  all gates passed"

# ----- step 2: bump Cargo.toml version ------------------------------------
log "step 2/${TOTAL_STEPS}: bumping ${CRATE_REL}/Cargo.toml to ${VERSION}"
current=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "$CRATE_TOML" 2>/dev/null || true)
if [[ -z "$current" ]]; then
    die_pre "no version found in $CRATE_TOML"
fi
if [[ "$current" == "$VERSION" ]]; then
    ok "  $CRATE_TOML already at $VERSION"
else
    sed -i "0,/^version[[:space:]]*=/{s/^version[[:space:]]*=.*$/version = \"${VERSION}\"/}" "$CRATE_TOML"
    ok "  $CRATE_TOML: $current → $VERSION"
fi

refresh_workspace_lock

# ----- step 3: close CHANGELOG [Unreleased] -------------------------------
log "step 3/${TOTAL_STEPS}: closing ${CRATE_REL}/CHANGELOG.md [Unreleased] → [${VERSION}]"
DATE=$(date -u +%Y-%m-%d)
# FIXED 2026-08-11 (audit HIGH): extracted the inline closer into the
# tested idempotent helper. Re-running after a partial release now leaves an
# existing version header byte-identical instead of duplicating it. A dry-run
# deliberately writes the local release surface so --abort has real work.
python3 "$SCRIPT_DIR/close-changelog.py" "$CHANGELOG" "$VERSION" "$DATE"
ok "  $CHANGELOG: [Unreleased] closed as [${VERSION}] - ${DATE} (or already closed)"

# ----- step 4: create release-notes file ----------------------------------
log "step 4/${TOTAL_STEPS}: creating ${CRATE_REL}/release-notes-v${VERSION}.md"
NOTES_REL="$CRATE_REL/release-notes-v${VERSION}.md"
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
cargo install dracon-system --version ${VERSION}
\`\`\`

## Docker / systemd

\`\`\`bash
# systemd unit (Linux)
curl -fsSL https://raw.githubusercontent.com/DraconDev/dracon-utilities/main/dracon-system/dracon-system-guard.service \\
    -o ~/.config/systemd/user/dracon-system-guard.service
systemctl --user daemon-reload
systemctl --user enable --now dracon-system-guard.service
\`\`\`

**Full Changelog**: https://github.com/DraconDev/dracon-utilities/compare/$(git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//' || echo "0.0.0")...v${VERSION}
EOF
    ok "  $NOTES_REL created"
fi

# ----- step 5: cargo publish --dry-run (sanity) ---------------------------
log "step 5/${TOTAL_STEPS}: cargo publish --dry-run (sanity check)"
run cargo publish -p "$CRATE_NAME" --dry-run --allow-dirty

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
        if ! "$SCRIPT_DIR/verify-install.sh" "$FIXTURE_ROOT/bin/dracon-system"; then
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
# The utility directory is parent-gitignored by design; force staging is
# scoped to the exact release surfaces and never uses `git add .`.
run git add -f -- "$CRATE_REL/Cargo.toml" "Cargo.lock" "$CRATE_REL/CHANGELOG.md" "$NOTES_REL"
# Idempotent re-run path: skip already-completed commit, tag, and GitHub
# release operations when a previous run failed later in the pipeline.
if [[ $DRY_RUN -eq 1 ]]; then
    run git commit --no-verify -m "release: v${VERSION}"
    run git tag "$TAG"
else
    if git diff --cached --quiet; then
        ok "  nothing to commit (release commit already exists)"
    else
        printf '   $ git commit --no-verify -m release: v%s\n' "$VERSION"
        git commit --no-verify -m "release: v${VERSION}"
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
        --title "v${VERSION}" \
        --notes-file "$NOTES"
else
    if gh release view "$TAG" >/dev/null 2>&1; then
        ok "  github release $TAG already exists"
    else
        printf '   $ gh release create %s\n' "$TAG"
        gh release create "$TAG" \
            --target main \
            --title "v${VERSION}" \
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
ok "  github:     https://github.com/DraconDev/dracon-utilities/releases/tag/${TAG}"
ok "════════════════════════════════════════════"

warn ""
warn "after 'cargo install dracon-system --version ${VERSION}', run the fixture check:"
warn "    ${CRATE_REL}/scripts/verify-install.sh"

if [[ $DRY_RUN -eq 1 ]]; then
    echo ""
    warn "This was a --dry-run. Local release surfaces were modified but no remote state was changed."
    warn "Run '${CRATE_REL}/scripts/release.sh --abort' to revert, or '${CRATE_REL}/scripts/release.sh ${VERSION} --yes' to execute for real."
fi
