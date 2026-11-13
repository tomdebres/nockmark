# Economics snapshot, 2026-11-13 (for the "which puzzle" write-up)

Fetched 2026-11-13 08:00 UTC (`fetched_at.txt`); the registry's economics
inputs were observed at 07:53:32 UTC.

- `economics.json`: `https://nockmark.xyz/economics`. ZK difficulty, AI
  difficulty, miner reward, NOCK price, and their sources (nockscan
  `mining-metrics`, CoinGecko).
- `nockscan-mining-metrics.json`, `nockscan-block-time-stats.json`:
  `https://nockscan.com/api/v1/…`. Network rates, block and work shares.
- `leaderboard-zk.json`, `leaderboard-ai.json`:
  `https://nockmark.xyz/leaderboard?track=…`. The verified rows.

`python3 writeup_tables.py` prints every figure in
`docs/writeups/2026-11-13-which-puzzle-should-this-card-mine.md` from these
files and `../../gpu/2026-11-13-pipelined-sweep/` (calibrated rates, luck-1
recovery, GPU power samples), and writes `tables.json`. The electricity
tariff (EIA, U.S. average residential, August 2026: 18.16 ¢/kWh) and the
community rental prices are constants in the script, with their sources.
