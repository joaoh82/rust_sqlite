# Phase 6 release engineering — plan *(revised)*

This is the design document for Phase 6 (CI/CD + releases). The goal
is to land every piece and verify it on GitHub *before* we start
Phase 7 (AI extensions).

**Revision history:**

- **v1** — initial plan with per-product version independence +
  direct push to main from release workflows.
- **v2** *(current)* — revised after owner review:
  - **Lockstep versioning** — one version bump covers every product.
  - **PR-based release flow** — required PR reviews on main stay in
    place; a release dispatch opens a PR with the version bumps, a
    human merges it, merge triggers the publish side.
  - **Trusted publishing (OIDC) confirmed** for PyPI + npm.
  - **Unsigned installers in Phase 6**; code signing moved to
    Phase 6.1 (later). Roadmap updated.
  - **Changelog generation** via GitHub's native auto-generated
    release notes (no `release-drafter` config needed for MVP).

## Goals

- **Lockstep semantic versioning** — every release bumps all
  products to the same `vX.Y.Z`. One dispatch, one PR, one set of
  tags, one coordinated publish across every registry. Simpler
  mental model, matches how most users think about "the SQLRite
  0.2.0 release."
- **Two-step release flow** compatible with required PR reviews on
  `main`:
  1. Manual dispatch → workflow opens a Release PR with version
     bumps across every product file.
  2. Human reviews + merges the PR → merge triggers the publish
     side, which tags + builds + publishes all artifacts.
- **CI on every PR** — build + test on Linux / macOS / Windows for
  every product, blocks merge if anything fails.
- **Publish to the canonical registry** for each language
  (crates.io, PyPI, npm). Go uses git tags (modules pull direct
  from git, no central registry push). Desktop + C FFI binaries ship
  as GitHub Release assets.
- **Reproducible**: anyone on the team (or your future self) can
  re-run a release workflow and get the same artifact.

## Constraints we're designing around

- **Go modules in subdirs** must be tagged exactly as
  `<subdir>/vX.Y.Z` — non-negotiable. Our Go SDK lives at `sdk/go/`
  so its tag format is `sdk/go/vX.Y.Z`.
- **Version duplication across files**. A lockstep release edits
  every manifest that carries a version string. The release
  workflow handles this automatically — see
  [Version bumping: exact file list](#version-bumping-exact-file-list)
  below. Humans never remember which files to touch.
- **Required PR reviews on `main`**: the release flow opens a PR
  with the version bumps; a human merges it after a quick glance.
  The actual tagging + publishing happens *after* the merge. No
  branch-protection bypass needed, no deploy keys, no ghost
  committer — just a PR that mutates ten files atomically.
- **Code signing for desktop**: macOS DMG + Windows MSI want real
  signing certs. Phase 6 ships unsigned — users see "unverified
  developer" warnings. Signing is its own follow-up
  ([Phase 6.1 in the roadmap](roadmap.md)).

## Per-product tag scheme (lockstep versioning)

Every release bumps every product to the same version `vX.Y.Z`. We
still emit per-product tags because Go's module system insists on
the `sdk/go/vX.Y.Z` format, and per-product tags let users filter
GitHub Releases by product ("show me every Python release").

| Product         | Tag format             | Publish target                                   |
|-----------------|------------------------|--------------------------------------------------|
| Rust engine     | `sqlrite-vX.Y.Z`       | crates.io + GitHub Release                       |
| C FFI shim      | `sqlrite-ffi-vX.Y.Z`   | GitHub Release (per-platform tarballs)           |
| `sqlrite-ask`   | `sqlrite-ask-vX.Y.Z`   | crates.io + GitHub Release                       |
| `sqlrite-mcp`   | `sqlrite-mcp-vX.Y.Z`   | crates.io + GitHub Release (per-platform binary tarballs) |
| Python SDK      | `sqlrite-py-vX.Y.Z`    | PyPI + GitHub Release                            |
| Node.js SDK     | `sqlrite-node-vX.Y.Z`  | npm (`@joaoh82/sqlrite`) + GitHub Release        |
| Go SDK          | `sdk/go/vX.Y.Z`        | Git tag (no registry) + GitHub Release assets    |
| WASM            | `sqlrite-wasm-vX.Y.Z`  | npm (`@joaoh82/sqlrite-wasm`) + GitHub Release   |
| Desktop app     | `sqlrite-desktop-vX.Y.Z` | GitHub Release (unsigned installers)           |
| **Meta**        | `vX.Y.Z`               | GitHub Release (links to the other nine; acts as the "this was release 0.2.0" anchor) |

All ten tags point at the same commit — the merge commit of the
release PR. The meta tag is the umbrella release users can link to
in announcements; the nine per-product tags are for tooling
(crates.io, Go module proxy, npm dist-tags, etc.) that expects a
specific format.

> **`sqlrite-ask` joined the lockstep wave in v0.1.17 (Phase 7g.1).** Gets
> its own tag and crates.io publish but ships in lockstep with everything
> else — same version every wave. `publish-ask` runs after `publish-crate`
> in `release.yml` because crates.io rejects publishes whose path-deps
> haven't yet resolved at the same version.

> **`sqlrite-mcp` joined the lockstep wave in Phase 7h (this commit).** Two
> new release jobs: `publish-mcp` (cargo publish to crates.io, sequenced
> after `publish-crate` + `publish-ask` because it depends on both) and
> `build-mcp-binaries` (per-platform binary tarballs for users who want
> to drop the executable on their PATH without installing a Rust
> toolchain). Same Cargo.toml version-bump pattern as the other crates.

## Version bumping: exact file list

The release workflow edits these files in a single commit (the
Release PR). Every file carries `"0.1.0"` today and needs the
matching new value:

| File                                     | Field                                       |
|------------------------------------------|---------------------------------------------|
| `Cargo.toml` (root)                      | `[package].version`                         |
| `sqlrite-ffi/Cargo.toml`                 | `[package].version`                         |
| `sqlrite-ask/Cargo.toml`                 | `[package].version`                         |
| `sqlrite-mcp/Cargo.toml`                 | `[package].version`                         |
| `sdk/python/Cargo.toml`                  | `[package].version`                         |
| `sdk/python/pyproject.toml`              | `[project].version`                         |
| `sdk/nodejs/Cargo.toml`                  | `[package].version`                         |
| `sdk/nodejs/package.json`                | `"version"` (top-level)                     |
| `sdk/wasm/Cargo.toml`                    | `[package].version`                         |
| `desktop/src-tauri/Cargo.toml`           | `[package].version`                         |
| `desktop/src-tauri/tauri.conf.json`      | `"version"` (top-level — Tauri reads this for installer names) |
| `desktop/package.json`                   | `"version"` (top-level)                     |
| `Cargo.lock`                             | auto-updated by `cargo build` after the above |

**Go** is not in this list — `sdk/go/go.mod` has no version field.
Go modules are versioned by their git tag exclusively.

**How the workflow edits these**: a single `scripts/bump-version.sh`
(lives in the repo, exercised by the release workflow) takes one
argument (the new version), uses `sed` + a tiny Python helper (for
the JSON files, where `sed` would be fragile against formatting)
to rewrite every entry. Idempotent — running it twice with the
same version is a no-op. Directly answers "do we bump the
Cargo.toml files?" — yes, all eleven of them.

The script is runnable locally too:

```bash
./scripts/bump-version.sh 0.2.0
cargo build   # regenerates Cargo.lock with the new versions
git diff      # preview what the release workflow would have committed
```

This lets you rehearse a release end-to-end without involving
GitHub.

## Workflows

### 1. `ci.yml` — continuous integration

- **Trigger**: `pull_request`, `push` to `main`.
- **Jobs** (all run in parallel, each with its own matrix):
  - **rust-ci** — matrix: `{ubuntu-latest, macos-latest, windows-latest}`.
    `cargo build --workspace`, `cargo test --workspace`,
    `cargo clippy --workspace --no-deps -- -D warnings`,
    `cargo fmt -- --check`.
  - **python-ci** — matrix: `{ubuntu, macos, windows}` × `{py3.9, 3.12}`.
    `maturin develop` in `sdk/python`, then `pytest`.
  - **nodejs-ci** — matrix: `{ubuntu, macos, windows}` × `{node 18, 20, 22}`.
    `npm ci`, `npm run build`, `npm test` in `sdk/nodejs`.
  - **go-ci** — matrix: `{ubuntu, macos}` (skip Windows for now — Go
    cgo on Windows needs mingw setup; not worth the complexity for
    the MVP). `cargo build --release -p sqlrite-ffi`, then
    `cd sdk/go && go test ./...`.
  - **wasm-ci** — `ubuntu-latest`. `wasm-pack build --target web` in
    `sdk/wasm`. Verify `.wasm` artifact exists, report its size so
    PRs surface size regressions.
  - **fmt-docs-ci** — cheap smoke that markdown files parse,
    `docs/_index.md` links all resolve, `cargo doc --no-deps`
    builds without warnings.

All jobs use cache actions (`actions/cache@v4` with `~/.cargo`,
`target/`, `node_modules/`) to keep PR turnaround fast.

**Completion signal**: CI turns green on the branch → PR mergeable.

Lockstep versioning collapses what was eight release workflows into
**two**. Every individual product-publish job still exists — it
just runs inside the umbrella release workflow as a parallel job,
not as its own file.

### 2. `release-pr.yml` — open a Release PR

The "prepare" half. Bumps every version string + opens a PR.
Doesn't publish anything.

- **Trigger**: `workflow_dispatch` with inputs:
  - `version` (string, required, semver) — e.g., `0.2.0`.
- **Steps**:
  1. Checkout main.
  2. Validate `version` is a valid semver + isn't lower than the
     current version (refuse downgrades).
  3. Create a new branch named `release/vX.Y.Z`.
  4. Run `scripts/bump-version.sh $VERSION` — rewrites every file
     listed in [Version bumping](#version-bumping-exact-file-list).
  5. `cargo build --workspace` to refresh `Cargo.lock`.
  6. Commit with message `release: v0.2.0` (the exact prefix is
     load-bearing — see workflow 3's trigger).
  7. Push the branch.
  8. Open a PR titled `Release v0.2.0` with an auto-generated body
     (changelog since the previous `v*` tag + "once merged, the
     publish workflow fires automatically").
- **Secrets**: none (uses `GITHUB_TOKEN` for the push + PR).

**Verification path**: you glance at the PR, check the diff is just
"bump ten version strings + refresh Cargo.lock + optional
changelog stub", review + merge.

### 3. `release.yml` — publish on Release PR merge

The "publish" half. Auto-fires on the release commit.

- **Trigger**:
  - `push` to `main` with commit message matching `^release: v` —
    the release PR's squash/merge commit lands here.
  - `workflow_dispatch` with a `version` input — fallback for when
    the auto-trigger needs to be re-run (runner flake, YAML bug).
- **Jobs** (run in parallel — products are independent at the
  publishing layer):
  - **tag-all** — reads the version from root `Cargo.toml` (source
    of truth), creates all eight tags pointing at the current
    commit: `sqlrite-vX.Y.Z`, `sqlrite-ffi-vX.Y.Z`,
    `sqlrite-py-vX.Y.Z`, `sqlrite-node-vX.Y.Z`, `sqlrite-wasm-vX.Y.Z`,
    `sdk/go/vX.Y.Z`, `sqlrite-desktop-vX.Y.Z`, `vX.Y.Z`. Pushes
    them. Runs *before* the publish jobs — if a tag already exists
    (accidental re-run, cosmic ray), the whole workflow aborts
    cleanly.
  - **publish-crate** — `cargo publish -p sqlrite-engine` the root
    crate to crates.io. (The crates.io name is `sqlrite-engine`, not
    `sqlrite`, because the short name was already taken by an
    unrelated project; the `[lib] name = "sqlrite"` keeps `use
    sqlrite::…` valid at import sites.) Creates GitHub Release
    `sqlrite-vX.Y.Z`.
  - **publish-ffi** — matrix build of `libsqlrite_c` for
    `{linux-x86_64, linux-aarch64, macos-universal, windows-x86_64}`.
    Packages each as a tarball containing the `.so`/`.dylib`/`.dll`,
    static `.a`, and generated `sqlrite.h`. Uploads to GitHub
    Release `sqlrite-ffi-vX.Y.Z`.
  - **publish-python** — `PyO3/maturin-action@v1` builds abi3-py38
    wheels for `{manylinux x86_64, manylinux aarch64, macOS
    universal, Windows x86_64}`. Publishes via **OIDC trusted
    publishing** to PyPI. Creates GitHub Release
    `sqlrite-py-vX.Y.Z` with wheel attachments.
  - **publish-nodejs** — napi-rs CLI builds `.node` binaries for
    `{linux x86_64/aarch64, macOS x86_64/aarch64, windows x86_64}`.
    Publishes to npm via **OIDC trusted publishing**. Creates
    GitHub Release `sqlrite-node-vX.Y.Z`.
  - **publish-wasm** — `wasm-pack build --target bundler --release`,
    then `wasm-pack publish` via OIDC. Creates
    `sqlrite-wasm-vX.Y.Z` GitHub Release.
  - **publish-go** — nothing to build on the Go side. Verifies
    `sdk/go/vX.Y.Z` was pushed correctly by `tag-all`. Pulls the
    per-platform `libsqlrite_c` tarballs produced by
    `publish-ffi` and attaches them to the Go release for users
    who want prebuilt C FFI alongside `go get`.
  - **publish-desktop** — `tauri-action@v0` builds Linux
    (`.AppImage`, `.deb`), macOS (`.dmg` universal), Windows
    (`.msi`). Uploads to GitHub Release `sqlrite-desktop-vX.Y.Z`.
    Unsigned — signing is Phase 6.1.
  - **finalize** (runs after all publishers succeed) — creates the
    umbrella GitHub Release `vX.Y.Z` with GitHub's native
    auto-generated release notes (enabled via
    `generate_release_notes: true` on `softprops/action-gh-release`).
    Body links to the seven per-product releases. This is the one
    users reference in announcements.

### How the two-workflow design plays with branch protection

- **Happy path**: dispatch `release-pr.yml` with version `0.2.0`.
  PR opens. You review + approve + merge. `release.yml` fires on
  the merge commit. All eight tags push. Seven publish jobs run
  in parallel. Umbrella GitHub Release finalizes. No branch-
  protection bypass needed, no deploy keys, no admin override.
- **Sad path — publish fails after tag push**: say
  `publish-python` fails on wheel upload. The tag
  `sqlrite-py-vX.Y.Z` is already on the remote. **Convention:
  never reuse a tag, always bump past.** Next release is
  `v0.2.1`, not a re-try of `v0.2.0`. Partial success is visible
  — the `sqlrite-vX.Y.Z` crate *did* publish, the Python wheels
  didn't, and both facts are recorded. Operators can fix the
  Python SDK and re-dispatch `release.yml` in manual mode at
  `v0.2.1`.
- **Sad path — an accidental `release: v…` commit message**: the
  auto-trigger fires. `tag-all` runs and finds the tags already
  exist (because the real release happened weeks ago). Workflow
  aborts with a clear "tag already exists" error. No damage.

## Secrets / one-time setup

With lockstep + OIDC-based trusted publishing, the only long-lived
secret left is crates.io. All the registry setup is web-UI clicks
captured in a separate runbook, `docs/release-secrets.md`, so the
future-you has a reference when something misbehaves six months
from now.

1. **crates.io** — needs a long-lived API token; Cargo doesn't
   support OIDC yet. Generate a scoped token (scope:
   `publish-new`, `publish-update`, name: `github-actions-release`).
   Store as repo secret `CRATES_IO_TOKEN`. Use `environment: release`
   scoping in the workflow so only jobs running in the `release`
   environment can read it.
2. **PyPI trusted publishing** — one-time config on PyPI's web UI
   for the `sqlrite` project: "Add trusted publisher" pointing at
   `joaoh82/rust_sqlite`, workflow `release.yml`, environment
   `release`. After that, no GitHub secret is needed — the
   workflow authenticates to PyPI via OIDC. Same pattern for
   TestPyPI (for dry-runs) if we decide we want that later.
3. **npm trusted publishing** — available via npm's newer "OIDC
   trusted publishing" system. One-time config on npm's web UI
   for the `@joaoh82/sqlrite` and `@joaoh82/sqlrite-wasm`
   packages. No `NPM_TOKEN` needed (after a one-time placeholder
   publish per `docs/release-secrets.md` §3a).
4. **GitHub Environments** — create one called `release` in repo
   settings → Environments. Add `joaoh82` as a required reviewer
   on the `release` environment. The publish jobs reference
   `environment: release`, so even though the release workflow
   auto-fires on merge, the publish step *pauses* until a human
   clicks "approve" in the GitHub UI. Belt + suspenders if the
   Release PR review wasn't as thorough as we'd like.
5. **GitHub Release** — no setup. `GITHUB_TOKEN` is automatic.
6. **Branch protection** — on `main`: require `ci.yml` green,
   require 1 approving review. No bypass configured — the
   release flow is PR-based so it doesn't need one.

`docs/release-secrets.md` captures the exact clicks needed in each
registry's web UI, in the order they need to happen. Written
first-person so future-you isn't re-discovering it at 2am.

## Implementation order

We land these one at a time, each in its own commit on this branch,
each verified on GitHub before moving on.

1. **6a — `scripts/bump-version.sh`** + docs for it. **✅ Landed.**
   Verified locally: `./scripts/bump-version.sh 0.1.1` produces a
   clean 10-file diff (+1 more from `Cargo.lock` after `cargo
   build`). `cargo test --lib` passes at the bumped version.
   Edge-case checks confirmed: invalid semver rejected, empty
   input rejected, prerelease versions accepted, idempotent on
   repeat runs, clean back-out via `git checkout`.
2. **6b — `ci.yml`** (CI on every PR). Lowest risk, highest
   signal. Open a PR with this plan doc + the bump script → CI
   fires → six green checks. Mergeable.
3. **6c — Branch protection + trusted-publishing one-time setup**
   (no code). Configure main to require `ci.yml` green + 1 review.
   Set up PyPI trusted publisher pointing at `release.yml`. Same
   for npm. Written into `docs/release-secrets.md` so future-you
   has a reference.
4. **6d — `release-pr.yml`** + `release.yml` as a **partial
   release** (only `tag-all` + `publish-crate` + `publish-ffi` +
   `finalize` wired up). Dispatch `release-pr.yml` at `0.1.1` →
   merge PR → `release.yml` fires → crates.io + GitHub Release
   for crate + FFI should materialize. This is the "skeleton
   publishes for real" milestone.
5. **6e — add `publish-desktop`** to `release.yml`. Bump to
   `0.1.2`, full release. Downloadable unsigned installers on the
   GitHub Release.
6. **6f — add `publish-python`** via maturin-action + OIDC. Bump
   to `0.1.3`. Wheels on PyPI.
7. **6g — add `publish-nodejs`** via napi-rs action + OIDC. Bump
   to `0.1.4`. `.node` binaries on npm.
8. **6h — add `publish-wasm`**. Bump to `0.1.5`. `sqlrite-wasm`
   on npm.
9. **6i — add `publish-go`** (just verifies the `sdk/go/vX.Y.Z`
   tag + attaches the FFI tarballs to the Go release). Bump to
   `0.1.6`. `go get github.com/joaoh82/rust_sqlite/sdk/go@v0.1.6`
   works.

After step 9 the tag list should look like:

```
v0.1.1 through v0.1.6 (umbrella)
sqlrite-v0.1.1 … sqlrite-v0.1.6
sqlrite-ffi-v0.1.1 … sqlrite-ffi-v0.1.6
sqlrite-desktop-v0.1.2 … sqlrite-desktop-v0.1.6
sqlrite-py-v0.1.3 … sqlrite-py-v0.1.6
sqlrite-node-v0.1.4 … sqlrite-node-v0.1.5 (wait, that's wrong)
```

Actually — the incremental releases only publish what's in
`release.yml` at that moment. Tags for products whose publish
jobs don't exist yet just don't get created. The bump script
still touches the version strings in every manifest, but the
tag-creation loop in `tag-all` only tags products whose publish
jobs are present.

**Alternative** — simpler: at each step the workflow tags *every*
product (even ones that aren't published yet) and creates an
empty GitHub Release for the products we haven't wired up.
Keeps the tag history consistent. I'll note this as an open
question in the verification notes; we'll decide at step 4.

Between each step: commit the workflow change, push, open PR,
CI runs on it, merge, then dispatch the release workflow at the
bumped version. Confirm the artifact, tick the box, move on.

## Verification strategy

Two stages per workflow:

1. **`pull_request` CI run** on the workflow's own PR. Catches
   YAML syntax errors, runner-setup mistakes, missing permissions,
   cache misconfigs, before anything is triggerable.
2. **Manual `workflow_dispatch` at a canary version**: once the
   workflow is merged, trigger it from the GitHub UI at a
   throwaway `0.1.x` version bump. We never ship broken public
   `0.2.0`s just to test the pipeline.

The release workflow *itself* doesn't take a `dry_run` flag —
that's what the two-step PR review is for. The Release PR is the
dry run: you look at the diff, decide it's sane, merge. If
anything downstream fails, we bump past to the next patch.

## Open questions

The Phase 6 v1 open questions have been resolved in this revision
(v2). For record:

1. **Branch protection**: ✅ **Decided — require PR reviews on
   main.** Hence the PR-based release flow in workflow 2/3.
2. **Trusted publishing (OIDC)**: ✅ **Yes, both PyPI and npm.**
   Captures the one-time web-UI setup in `release-secrets.md`.
3. **Linux aarch64 runners**: ✅ **Yes** — public repo, so
   `ubuntu-24.04-arm` runners are free.
4. **Desktop code signing**: ✅ **Unsigned in Phase 6** — tracked
   as Phase 6.1 in the roadmap for later.
5. **Version independence**: ✅ **Lockstep** — single `version`
   input bumps every product. Informs the whole two-workflow
   design above.
6. **Tag cleanup on failed release**: ✅ **Never reuse a tag,
   always bump past.** Documented convention.
7. **New** — **Incremental-publish tag policy**: when we land the
   release workflow with only some publish jobs wired up (steps
   4–9 of the implementation order), do we tag *only* the
   products whose publish jobs exist, or *every* product even
   though some aren't published? Recommendation: tag every
   product from day one so the tag history is consistent, but
   create empty GitHub Releases for the not-yet-wired ones
   (filled in at the next bump).

## What's *not* in this phase

For scope clarity, the following are **explicitly out** of Phase 6:

- Code signing (Apple Developer cert + Windows code-sign cert) —
  deferred to **Phase 6.1** on the roadmap.
- Richer changelog generation beyond GitHub's native
  `generate_release_notes: true` (which groups by PR labels /
  conventional commits). If we want a nicer changelog we can add
  `release-drafter` later — the GitHub native version is good
  enough for MVP.
- Dependency update bot (dependabot / renovate) — would be nice
  but it's meta-tooling, not release tooling.
- Nightly / canary builds — we ship tagged versions only.
- Benchmarking in CI — Phase 7-ish.
- OPFS-backed WASM persistence (Phase 5g follow-up).
- Phase 5f Rust crate polish (deferred — happens alongside 6d's
  first `cargo publish` run).
