# Continuous integration

The `CI` workflow runs on pushes to `main`, pull requests targeting `main`
(including forks), and manual dispatches. Each run checks stable Rust with both
bundled SQLite and the system SQLite library:

- `cargo test --locked --all-targets` (unit, integration, and example targets)
- `cargo test --locked --doc`
- `cargo clippy --locked --all-targets -- -D warnings`

Both SQLite jobs use `--no-default-features --features <sqlite-feature>`.
Formatting is not enforced yet: the repository has existing rustfmt drift.

## Fork PRs and trusted workflow definitions

Automatic `pull_request` runs test GitHub's synthetic merge commit: the PR
combined with its target branch. They do **not** only test `main`, nor does the
workflow definition necessarily come only from `main`: PR changes to workflows
can affect these runs too.

Do not switch test execution to `pull_request_target`. That event uses the
trusted default-branch workflow context, but executing fork code there can
expose privileged credentials or poison caches. This workflow instead uses
GitHub-hosted runners, a read-only token, no repository secrets, no persisted
checkout credentials, and no shared build caches. Keep those restrictions when
extending it; never run unreviewed forks on a self-hosted runner.

To test a PR with the **workflow definition from `main`**, including a PR that
was opened before CI existed:

1. Open **Actions → CI → Run workflow**.
2. Select branch **main**.
3. Enter the PR number (for example `1`) in `pull_request`, then run it.
4. Inspect both SQLite jobs. The checkout log identifies the tested merge SHA.

Equivalent GitHub CLI command:

```sh
gh workflow run ci.yml --repo koriwi/libopod --ref main -f pull_request=1
```

Manual dispatch from another branch is rejected. An omitted PR number tests the
selected `main` revision instead. A conflicting or unavailable PR merge ref
fails checkout rather than silently testing the base branch. The manual run
appears under Actions; it does not replace the automatic PR status checks.
Rerun after either branch changes: a past green result covers only the revision
actually checked out.

## Repository settings

After pushing the workflow:

- Enable GitHub Actions under **Settings → Actions → General**. Allow the pinned
  `actions/checkout` and `dtolnay/rust-toolchain` actions if an allowlist is used.
- Keep fork workflow tokens read-only and do not enable sending secrets to
  forks. A first-time contributor's automatic run may require approval.
- Under **Settings → Rules → Rulesets** (or branch protection), protect `main`
  and require `Tests (bundled-sqlite)` and `Tests (system-sqlite)` before merge.
  Require branches to be up to date so checks include the latest base changes.
  The check names may only become selectable after their first run.
- Review changes to `.github/workflows/` carefully. Required status names alone
  do not make a PR's workflow immutable. If your GitHub organization/plan offers
  required-workflow rules, use a trusted workflow source for stronger enforcement.

These settings are separate from the committed workflow and must be configured
by a repository administrator. Merely adding CI does not block failing merges.

## Public versus private tests

Public CI checks out only tracked files. Synthetic database/artwork and recovery
tests and public HASHAB vectors run without physical hardware. Tests depending
on private `backup_7g`, attached device dumps, `LIBOPOD_PRIVATE_FIXTURE`, or
`LIBOPOD_TEST_NANO2_SNAPSHOT` skip when those inputs are absent (some currently
return early rather than showing Rust's `ignored` marker).

A green public run therefore does not certify the optional private-fixture
checks or physical playback. Do not upload private device dumps or credentials
to make fork PR tests run; retain those checks in a trusted local environment.
