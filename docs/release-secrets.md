# Release secrets runbook

One-time setup for the Phase 6 release pipeline. Everything here
is configured through web UIs on each registry — not in code —
because the state lives on crates.io / PyPI / npm / GitHub, not
in this repo.

Written first-person so future-you isn't re-discovering it at 2am.
Each step has a "verify" line you can run to confirm it took.

**Read order: top to bottom.** The dependencies run one way:

- Sections 1 + 4 + 5 can be done right now (they don't reference
  workflow files).
- Sections 2 + 3 (trusted publishers) are safe to do now too —
  they'll point at `release.yml` which lands in Phase 6d, and
  the publisher will sit idle until the first workflow run that
  matches.

---

## 1. crates.io API token → `CRATES_IO_TOKEN` repo secret

**Why this exists and not OIDC:** Cargo / crates.io doesn't
support OIDC trusted publishing yet (they're working on it
upstream). So crates.io is the only registry still needing a
long-lived token. Everything else (PyPI, npm) is OIDC.

**Steps:**

1. Log in at <https://crates.io/>.
2. Click your avatar → **Account Settings** → **API Tokens** →
   **New Token**.
3. Fill in:
   - **Name**: `github-actions-release`
   - **Expiration**: 1 year (set a calendar reminder to rotate).
   - **Scopes**: check only `publish-new` and `publish-update`.
     The other scopes (`yank`, `change-owners`) aren't needed for
     releases.
   - **Crates**: leave blank for now (scope-to-crate lands when
     we first publish; until then we need the token broad enough
     to create the `sqlrite` crate).
4. Click **Create**. Copy the token — **this is the only time
   it's visible**.
5. Back in GitHub: **Settings** → **Secrets and variables** →
   **Actions** → under *Environment secrets* for the `release`
   environment (created in section 4 below), click **New
   secret**. Name: `CRATES_IO_TOKEN`. Value: paste the token.
   - Do NOT add this as a repo-level secret. Scoping to the
     environment means only jobs with `environment: release` can
     read it, and those jobs require maintainer approval before
     running (section 4).

**Verify**: run `gh secret list --env release` (GitHub CLI) and
confirm `CRATES_IO_TOKEN` appears. Or: in the GitHub UI, open the
`release` environment and check the secrets list.

---

## 2. PyPI trusted publisher

**Why trusted publishing:** no long-lived PyPI token. Every
workflow run authenticates via short-lived OIDC tokens that
GitHub Actions mints on the fly. Rotation is automatic; there's
nothing to leak.

**Package name**: `sqlrite` on PyPI.

### 2a. Reserve the name

1. Log in at <https://pypi.org/>.
2. Search for `sqlrite` — confirm it's not already taken.
   (If it is, we'll need a different name; update
   `sdk/python/pyproject.toml`'s `name` field + the
   `napi.name` field in `sdk/nodejs/package.json` for consistency
   and file an issue.)
3. You can optionally pre-register an empty project via
   <https://pypi.org/manage/projects/> → **Register a project**.
   Not strictly required — the first successful publish creates
   the project record. Pre-registering lets you configure the
   trusted publisher before the first release, which is what the
   next steps do.

### 2b. Add the publisher

1. Go to <https://pypi.org/manage/account/publishing/>.
2. Under **Add a new pending publisher**, fill in:
   - **PyPI Project Name**: `sqlrite`
   - **Owner**: `joaoh82`
   - **Repository name**: `rust_sqlite`
   - **Workflow name**: `release.yml` (the filename, not the
     path — PyPI looks for it at
     `.github/workflows/release.yml`)
   - **Environment name**: `release` (must match the name we
     create in section 4)
3. Click **Add**. The publisher appears as "pending" until the
   first successful OIDC-authenticated publish; at that point
   PyPI swaps it to active automatically.

**Verify**: the publisher shows up in the list on the same page
with status "pending". Once Phase 6d runs its first canary
release, status flips to "active".

---

## 3. npm trusted publishers (two packages)

**Why two:** we publish `@joaoh82/sqlrite` (Node.js bindings from
`sdk/nodejs/`) and `@joaoh82/sqlrite-wasm` (browser bindings from
`sdk/wasm/`) as separate npm packages. Each needs its own
trusted-publisher record.

**Why both are scoped:** npm's registry rejects unscoped names
that are too similar to existing popular packages — `sqlrite` is
levenshtein-distance 1 from `sqlite`/`sqlite3`, and
`sqlrite-wasm` would be distance 1 from `sqlite-wasm`. Scoping
under `@joaoh82` (the author's npm user scope) bypasses the
check entirely — same pattern as `@napi-rs/*`, `@swc/core`,
`@aws-sdk/*`. We learned this the hard way on the Node package
during the v0.1.5 canary; for the WASM package we went scoped
preemptively in Phase 6h.

### 3a. Publish a placeholder for each scoped package

**npm requires the package to exist before you can configure a
trusted publisher for it** (no PyPI-style "pending publisher"
flow as of late 2025). The bootstrap is a one-time manual
publish of an empty `0.0.0` placeholder using your local
credentials. Scoped packages under your own user scope are
auto-owned, so no separate name reservation is needed beyond
the publish itself.

For each of `@joaoh82/sqlrite` and `@joaoh82/sqlrite-wasm`:

```bash
mkdir /tmp/scoped-placeholder && cd /tmp/scoped-placeholder
cat > package.json <<'JSON'
{
  "name": "@joaoh82/sqlrite",
  "version": "0.0.0",
  "description": "Placeholder — real package ships from rust_sqlite CI",
  "license": "MIT"
}
JSON
npm login   # if not already
npm publish --access public
# Repeat for @joaoh82/sqlrite-wasm — change the name field.
```

The placeholder is harmless; the first CI release publishes a
real `0.X.Y` over the top.

### 3b. Trusted publisher for each package

For each placeholder you just published:

1. Go to the package's settings page:
   - <https://www.npmjs.com/package/@joaoh82/sqlrite/access>
   - <https://www.npmjs.com/package/@joaoh82/sqlrite-wasm/access>
2. Find the **Trusted Publisher** section (under Settings, not
   the package list).
3. **Add publisher**:
   - **Publisher**: GitHub Actions
   - **Organization or user**: `joaoh82`
   - **Repository**: `rust_sqlite` *(repo basename, not
     `joaoh82/rust_sqlite` — npm prepends the owner field)*
   - **Workflow filename**: `release.yml` *(basename, not
     `.github/workflows/release.yml`)*
   - **Environment**: `release` *(case-sensitive — must match the
     `environment: release` block on the publish-* jobs in the
     workflow)*
4. Save.

**Verify**: each package's settings page should show the
trusted publisher with status "active" after the first
successful CI publish.

**Why every field matters:** the OIDC subject claim our workflow
sends to npm is `repo:joaoh82/rust_sqlite:environment:release`.
npm builds the matcher from the form fields above; if any field
disagrees with the OIDC claim, npm responds 404 ("OIDC token
exchange error - package not found"), which is npm's misleading
way of saying "no trusted publisher record matches your token's
claims". Burned us once on v0.1.7 (typo'd repo name in the
form); kept the form field reference here so the next person
doesn't have to re-debug.

---

## 4. GitHub `release` environment

**Why an environment:** gives us a second human-in-the-loop gate
between the Release PR merge and the actual registry publishes.
Even if the PR got auto-merged (say we later wire up a bot), the
maintainer still has to click "Approve and deploy" before any
job that writes to a registry runs.

**Steps:**

1. Go to <https://github.com/joaoh82/rust_sqlite/settings/environments>.
2. Click **New environment** → name it `release` → **Configure environment**.
3. **Required reviewers**: check the box, add yourself (`joaoh82`).
   Optional: add any other maintainers who should be allowed to
   approve publishes.
4. **Wait timer**: leave at 0 (no artificial delay).
5. **Deployment branches and tags**: restrict to `main` —
   publishes should only ever run off a commit that landed on
   `main`. Select **Selected branches and tags** → add a rule
   for `main`.
6. Save.

**Secrets live here, not at the repo level.** When you add
`CRATES_IO_TOKEN` (section 1), use this environment's
*Environment secrets* section. Same for any temporary
`NPM_TOKEN` (section 3b).

**Verify**: in the `release` environment page you should see:
- Required reviewer: yourself
- Deployment branches: main
- Secrets: `CRATES_IO_TOKEN` (+ `NPM_TOKEN` until OIDC takes over)

---

## 5. Branch protection on `main`

**Why:** this is what turns CI from "nice to have" into "actually
blocks mistakes from reaching main". Also what makes the
PR-based release flow necessary (hence the whole workflow-2 +
workflow-3 split — see [release-plan.md](release-plan.md)).

**Steps:**

1. Go to
   <https://github.com/joaoh82/rust_sqlite/settings/branches>.
2. Under **Branch protection rules**, click **Add rule** (or
   **Add classic branch protection rule** on newer GitHub UIs).
3. **Branch name pattern**: `main`
4. **Require a pull request before merging**: ✓
   - **Require approvals**: ✓, set to 1.
   - **Dismiss stale pull request approvals when new commits are
     pushed**: optional (helpful for team workflows; solo-dev,
     skip).
   - **Require review from Code Owners**: skip (no CODEOWNERS
     file yet).
5. **Require status checks to pass before merging**: ✓
   - **Require branches to be up to date before merging**:
     optional (tightens but slows down; enable once there are
     multiple contributors).
   - **Status checks that are required**: add each CI job name
     from `ci.yml`. As they appear in GitHub's dropdown after
     the first CI run:
     - `rust (ubuntu-latest)`
     - `rust (macos-latest)`
     - `rust (windows-latest)`
     - `rust lint`
     - `python-sdk (ubuntu-latest)`
     - `python-sdk (macos-latest)`
     - `python-sdk (windows-latest)`
     - `nodejs-sdk (ubuntu-latest)`
     - `nodejs-sdk (macos-latest)`
     - `nodejs-sdk (windows-latest)`
     - `go-sdk (ubuntu-latest)`
     - `go-sdk (macos-latest)`
     - `wasm-build`
     - `desktop-build`
6. **Require conversation resolution before merging**: ✓ —
   prevents merging a PR with unresolved review comments.
7. **Require linear history**: optional. Nicer `git log`; merge
   commits are still fine without it.
8. **Do not allow bypassing the above settings**: leave
   unchecked (you'll want admin bypass available for emergency
   fixes).
9. Save.

**Verify**: open a draft PR from any branch. The merge button
should be disabled until CI passes + you have a review.

---

## Verification checklist

Run through this once everything above is done:

- [ ] `gh secret list --env release` shows `CRATES_IO_TOKEN`.
- [ ] The `release` environment requires you as a reviewer and
      restricts to `main`.
- [ ] PyPI trusted-publisher page shows `rust_sqlite` /
      `release.yml` / `release` pending for the `sqlrite`
      project.
- [ ] npm trusted-publisher page shows the same for both
      `@joaoh82/sqlrite` and `@joaoh82/sqlrite-wasm` (assuming
      the placeholders are published per §3a — if not, section
      3a applies).
- [ ] Branch protection on `main` requires 14 status checks + 1
      review.
- [ ] Open a dummy PR — the "Merge" button is greyed out until
      CI green + review given.

---

## What isn't here (deferred follow-ups)

These are explicitly out of Phase 6c scope but worth capturing so
they aren't forgotten:

- **Apple Developer ID certificate** for macOS Tauri DMG signing
  → Phase 6.1. Procurement: $99/year at
  <https://developer.apple.com/programs/>.
- **Windows code-signing certificate** for Tauri MSI signing →
  Phase 6.1. Procurement: ~$300/year from Sectigo / DigiCert /
  etc.; EV certs are pricier but skip the Windows SmartScreen
  "this publisher is unknown" warning.
- **Dependabot or renovate** to keep deps fresh — meta-tooling,
  not release tooling. Can land any time.
- **CODEOWNERS** file once there are multiple maintainers.
- **Required review from multiple reviewers** once there are
  multiple maintainers.

---

## Rolling back

If something goes sideways:

- **Revoke `CRATES_IO_TOKEN`**: crates.io → Account Settings →
  API Tokens → **Revoke**. Generate a new one.
- **Remove a trusted publisher**: on the registry's publisher
  management page, delete the entry. The next workflow run will
  fail to authenticate, which is usually what you want when
  rolling back.
- **Bypass branch protection for a hotfix**: if you need to push
  directly to main for an emergency, GitHub requires admin to
  toggle the "Include administrators" option off in branch
  protection, push, then re-enable. Document the reason in a
  commit message.
