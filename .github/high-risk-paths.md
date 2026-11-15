# High-risk paths

A pull request that touches one of these, or that comes from outside, needs
the maintainer's approval of its head commit before it can merge: the "Risk
tier" check (`scripts/risk-tier.mjs`) reads this table, and
`.github/CODEOWNERS` is generated from it (`node scripts/risk-tier.mjs
codeowners > .github/CODEOWNERS`). Globs are from the repo root; "deleted" rows
count only when a file is deleted or renamed away.

<!-- high-risk-paths -->
| Path | Counts | Why |
|---|---|---|
| `.github/**` | any | CI, review, Risk tier, Publish check, CODEOWNERS and this table: the gates themselves. |
| `scripts/risk-tier.mjs` | any | The matcher Risk tier runs. |
| `scripts/publish-check.mjs` | any | What keeps private details out of this public repo. |
| `test/**` | deleted | A deleted test is a weaker spec. |
<!-- /high-risk-paths -->
