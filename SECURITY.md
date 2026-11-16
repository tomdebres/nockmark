# Security

The registry's promise is that a published rate is a lower bound nobody can
inflate. Anything that breaks that promise is a security bug.

## Reporting

Report it privately, through GitHub's private vulnerability reporting: the
repository's **Security** tab, then **Report a vulnerability**
(<https://github.com/tomdebres/nockmark/security/advisories/new>). Please
don't open a public issue or pull request for it. The report stays private
until a fix is out, and the conversation happens on the advisory.

A useful report says what you did, what the registry accepted, and the rate
or row that came out of it. A proof of concept against a registry you run
locally is best (`registry/` builds and runs on its own); please don't load
or flood the live instance at nockmark.xyz to show a bug.

## In scope

- **The registry's verification**: challenge minting, the checks on ZK `%5`
  proofs and AI-PoW certificates and artifacts, nonce and challenge reuse, and
  the rules a run is ranked under.
- **Anything that lets someone inflate a rate**, or put a row on the board
  that the work didn't earn: getting a proof accepted that shouldn't be,
  counting one win twice, precomputing work for a challenge, or moving the
  server-side window (`submitted_at − issued_at`) a rate is computed from.
- The registry server itself, as deployed from this repository (its HTTP API,
  the run stores, the leaderboard and economics pages).

## Out of scope

- Self-reported fields. The hardware descriptor and `prover_version` are
  displayed as reported and never verified; the README says so.
- Bugs in Nockchain itself, including its provers, verifiers and kernels.
  Report those to the [Nockchain project](https://github.com/zorp-corp/nockchain).
  If one lets a bad proof through the registry too, say so here as well.
- Volume attacks on the hosted instance (rate limits, denial of service).
