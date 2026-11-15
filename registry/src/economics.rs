//! Measured rate → estimated daily return, for both tracks, in NOCK and
//! (when a price is available) in USD.
//!
//! ## ZK track
//!
//! zkPoW is an attempt lottery: with `difficulty` = expected attempts per ZK
//! block, a miner making `r` attempts/sec expects `r × 86400 / difficulty`
//! blocks/day, each paying `block_reward_nock`.
//!
//! What an attempt IS changed at height 147,500 (Anthropos, protocol 017, ZK
//! proof version %5). Before it, every attempt was a complete STARK proof,
//! so attempts/sec was proofs/sec — the unit the ZK board ranks. From it, an
//! attempt is ONE Tip5 hash of proof object 0 (`%puzzle [commitment nonce
//! len p]`), and a full proof is built only for the winning nonce. Network
//! difficulty now counts Tip5 hashes (~2^46.5 per block in Nov 2026). The ZK
//! board's ranked rows are verified Tip5 hashes/sec (the v5 track,
//! `crate::zkv5`) and are priced with exactly this formula; its pre-Anthropos
//! rows are proofs/sec, which no longer prices anything, and carry NO
//! earnings estimate ([`ZK_PROOF_ROWS_UNPRICED`]).
//!
//! ## AI track
//!
//! AI-PoW is the same lottery counted in a different unit. Consensus prices
//! heterogeneous AI work in MAC-equivalents, and expected MAC-equivalents per
//! AI block is `2^256 / (T_ai + 1)` **whatever tile shape the miner picked**
//! — invariant D2 of `ai-pow/src/difficulty.rs`, and literally `+ai-pow-work`
//! in `tx-engine.hoon`. A machine at `R` MAC-equivalents/sec therefore expects
//! `R × 86400 × (T_ai + 1) / 2^256` blocks/day, each paying the SAME
//! `block_reward_nock`: post-Logos `+build-ai-candidate` rebuilds the coinbase
//! with the same `+new-with-fund-share` 80/20 split, so an AI block and a ZK
//! block pay a miner identically.
//!
//! Anchor check, in the tests: Logos anchors AI ASERT at `T_ai = 2^192`, which
//! `changelog/protocol/016-logos.md` states is `2^64` MAC-equivalents per
//! block — and `2^256 / (2^192 + 1)` is `2^64`.
//!
//! ## USD
//!
//! A NOCK/day figure is only half an answer for someone deciding whether to
//! plug a machine in. USD/day is `est_nock_per_day × nock_usd`, from a spot
//! price polled on the same 10-minute tick as difficulty. The price feed is
//! optional and every USD figure is OMITTED when it is unavailable — never
//! zero, never a stale number without its observation time attached. A reader
//! who sees no USD column knows exactly as much as we do.
//!
//! ## What the operator supplies
//!
//! Env seed: NOCKMARK_DIFFICULTY, NOCKMARK_BLOCK_REWARD_NOCK (miner share).
//! Both are refreshed online when a source is configured — every 10 minutes,
//! primary first, fallback only when the primary fails:
//!   - NOCKMARK_ECON_URL — a GET source. Either nockscan's
//!     `/api/v1/mining-metrics` (detected by its `log2Difficulty` field:
//!     ZK and AI difficulty, the block reward and the chain's own exchange
//!     rate, no key), or any JSON with a top-level numeric "difficulty"
//!     (ZK only);
//!   - NOCKMARK_ECON_RPC_URL + NOCKMARK_ECON_API_KEY — a NockBlocks-style
//!     JSON-RPC endpoint: POST getTip with an x-api-key header, difficulty
//!     derived from the tip's target, one track per tip.
//! Legacy: NOCKMARK_ECON_URL together with NOCKMARK_ECON_API_KEY and no
//! NOCKMARK_ECON_RPC_URL is read as the RPC source, as before M7.
//! Optional overrides: NOCKMARK_BLOCK_TIME_SECS, NOCKMARK_AI_BLOCK_TIME_SECS,
//! NOCKMARK_PRICE_URL.
//!
//! Unset → the /economics endpoint reports itself unconfigured and the
//! leaderboards omit estimates. Estimates are estimates: see
//! [`EXCLUDED_FROM_ESTIMATES`] for exactly what they do not account for.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::RwLock;

/// Everything needed to turn a measured rate into an estimated daily return,
/// plus the observation times that say how fresh each input is.
///
/// Clone rather than Copy: the price source URL and the AI target are carried
/// verbatim so `/economics` can publish the exact inputs a reader would need
/// to recompute any figure we print. The struct is read once per request under
/// an `RwLock`, so the clone is noise.
#[derive(Debug, Clone, Serialize)]
pub struct EconParams {
    /// Expected attempts per ZK block at current network difficulty — Tip5
    /// hashes since Anthropos (height 147,500), complete proofs before.
    pub difficulty: f64,
    /// Where the current `difficulty` came from (a URL), `None` for the env
    /// seed. Two sources can now feed it, so the figure says which one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty_source: Option<String>,
    /// Current eon's block reward in NOCK, already the 80% miner share.
    /// Paid identically by a ZK block and an AI block — one emission
    /// schedule, one coinbase split.
    pub block_reward_nock: f64,
    /// When [`Self::block_reward_nock`] was last read from a source, unix
    /// secs; `None` while it is still the env seed. The reward steps once per
    /// eon (2048 → 1536 at height 170,501), and a seed nobody refreshed is
    /// exactly how the board kept paying 1638.4 for weeks after that step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reward_observed_at: Option<u64>,
    /// When [`Self::difficulty`] was observed, unix secs. `None` while the
    /// value is the operator-supplied env seed and no tip has been read yet;
    /// an estimate built on a seed no refresh has confirmed should say so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty_observed_at: Option<u64>,
    /// The timestamp of the BLOCK that difficulty came from, when the source
    /// reports one.
    ///
    /// Not redundant with `difficulty_observed_at`, and the difference is the
    /// whole point: we re-read the tip every ten minutes, so "observed 3
    /// minutes ago" stays true even while the chain — or the explorer
    /// indexing it — has not moved in a week. Only the block's own timestamp
    /// can say that, and on 2026-08-22 it was saying exactly that: the tip
    /// nockblocks served was 8 days old.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty_tip_timestamp: Option<u64>,
    /// AI-PoW difficulty, `None` until an `ai-pow` tip has actually been
    /// observed. There is no seed for this one and no way to derive it from
    /// the ZK figure, so "not yet seen" is a real state that must stay
    /// visible — every AI earnings figure is omitted while it holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiDifficulty>,
    /// Spot NOCK/USD, `None` when the feed is disabled or has never
    /// succeeded. Every USD figure is omitted while it holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<NockPrice>,
}

/// One observed AI-PoW difficulty.
#[derive(Debug, Clone, Serialize)]
pub struct AiDifficulty {
    /// Expected MAC-equivalents per AI block: `2^256 / (T_ai + 1)`.
    pub mac_equiv_per_block: f64,
    /// The tip's raw target, verbatim as the node spelled it, when the source
    /// gives one (the RPC tip does; nockscan publishes log2 of the difficulty
    /// instead). Published so a reader can redo `2^256 / (T + 1)` themselves
    /// rather than take our word for the division.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The URL this observation came from.
    pub source: String,
    /// Unix secs. Since Anthropos AI blocks are ~70% of the chain (214 s AI
    /// ideal against ZK's 500 s), but either track's figure can lag the other
    /// on an RPC source that only sees the tip — which is why it is carried
    /// per-difficulty rather than once for the whole struct.
    pub observed_at: u64,
    /// The AI block's own timestamp, when the source reports one. Here this
    /// is close to `observed_at` by construction (we only learn an AI
    /// difficulty when an AI block is AT the tip), but it is what pins the
    /// figure to chain time rather than to our polling schedule.
    pub tip_timestamp: Option<u64>,
}

/// One observed NOCK spot price.
#[derive(Debug, Clone, Serialize)]
pub struct NockPrice {
    pub usd: f64,
    /// The URL it came from — the "source" half of "price with source".
    pub source: String,
    /// Unix secs.
    pub observed_at: u64,
}

/// What these estimates deliberately do NOT account for.
///
/// Published verbatim on `/economics` rather than left to a footnote,
/// because the gap between "NOCK/day at current difficulty" and "money" is
/// where a potential miner gets hurt. No wattage figure appears here: we have
/// no measured power draw for any machine on these boards, and inventing one
/// would be worse than omitting it.
pub const EXCLUDED_FROM_ESTIMATES: [&str; 5] = [
    "electricity: no power draw is modelled, because we have no measured wattage for any machine on these boards",
    "hardware capital and its depreciation",
    "pool fees, and the variance a solo miner at these rates actually faces (a fraction of a block per day is not a block per day)",
    "difficulty drift: every figure is a snapshot at the observed-at times above, and difficulty moves as hashrate joins or leaves",
    "price drift: the same snapshot caveat, on a spot price that can move further in an hour than difficulty does in a week",
];

/// Largest tip5 digest as an atom: five base-p digits of p−1, i.e. p⁵−1,
/// where p = 2⁶⁴ − 2³² + 1 is the Goldilocks prime. This is nockchain's
/// max-target-atom (hoon/common/tx-engine-0.hoon); a block's work — the
/// expected proof attempts it took — is max-target/(target+1), Bitcoin's
/// GetBlockProof shape. f64 precision (~1e-16 relative) is far below the
/// noise floor of an earnings estimate.
const MAX_TIP5_ATOM: f64 = {
    let p = 18_446_744_069_414_584_321u128 as f64; // 2^64 − 2^32 + 1
    p * p * p * p * p
};

/// 2^256: the AI-PoW jackpot hash space. The jackpot is a 32-byte digest read
/// as a 256-bit integer, so expected attempts to land at or under a threshold
/// `Θ` is `2^256/(Θ+1)` — the same GetBlockProof shape as above, over a
/// different digest width. Written as a product of exact halves rather than a
/// 78-digit decimal literal so it is verifiably the power of two it claims to
/// be (and it is exactly representable: f64 carries powers of two to 2^1023).
const TWO_POW_256: f64 = {
    let half = (1u128 << 127) as f64; // 2^127
    half * half * 4.0
};

/// `max-ai-target-atom`, 2^232 − 1 (hoon/common/tx-engine-0.hoon:122;
/// `AI_POW_MAX_CONSENSUS_TARGET` in ai-pow/src/difficulty.rs). Consensus
/// refuses an AI target at or above this, so it is also the cleanest guard
/// against reading a ZK target as an AI one: a real ZK target is on the ~2^291
/// scale and cannot pass, and a real AI target cannot fail.
const MAX_AI_TARGET_ATOM: f64 = {
    let half = (1u128 << 116) as f64; // 2^116
    half * half // 2^232; the −1 is far below f64 resolution here
};

/// Ideal ZK block interval, overridable via NOCKMARK_BLOCK_TIME_SECS.
///
/// 500 s since Anthropos (height 147,500, `tx-engine.hoon`), which swapped
/// Logos's allocation: ZK went 214 s → 500 s and AI 500 s → 214 s, i.e. ~30%
/// ZK / ~70% AI blocks at the same ~150 s combined cadence. (150 s under
/// Aletheia, 214 s under Logos.) Observed over the 576 blocks to 181,400:
/// 502.7 s. Only used for the derived network-rate estimate, never for
/// ranking.
pub fn block_time_secs() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("NOCKMARK_BLOCK_TIME_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(500.0)
    })
}

/// Ideal AI ("puzzle") block interval, overridable via
/// NOCKMARK_AI_BLOCK_TIME_SECS.
///
/// 214 s since Anthropos (it was 500 s under Logos): `(1/214) / (1/214 +
/// 1/500)` = 70.03% of blocks, the share the protocol targets — it is not a
/// separately-tuned constant. Observed over the 576 blocks to 181,400: 218.1 s.
///
/// Deliberately NOT read off the explorer tip: explorers have published
/// interval fields that did not match consensus before (NockBlocks reported
/// 375 s under Logos, a piece of the ZK anchor expression `375 · 2^291 / 214`).
/// The protocol source is the authority here; explorers are trusted only for
/// difficulty, which can be range-checked.
pub fn ai_block_time_secs() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("NOCKMARK_AI_BLOCK_TIME_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(214.0)
    })
}

/// Consensus exchange rate between the two puzzles' work units: MAC-equivalents
/// per ZK attempt, height-selected in `tx-engine.hoon`. Since Anthropos a ZK
/// attempt is one Tip5 hash; the current rate applies from height 154,500:
///
/// | heights            | MAC-equivalents per ZK attempt | ZK attempt        |
/// |--------------------|--------------------------------|-------------------|
/// | 126,000 – 147,499  | 25,750,000,000                 | a complete proof  |
/// | 147,500 – 154,499  | 1,028,807                      | a Tip5 hash       |
/// | 154,500 –          | 14,285,714                     | a Tip5 hash       |
///
/// It is what the AI board's `zk_attempt_equiv_per_sec` divides by, and what
/// nockscan publishes as `workNormalization.macsPerZkUnit`.
pub const MAC_EQUIV_PER_ZK_ATTEMPT: f64 = 14_285_714.0;

/// How the ZK board is priced, published verbatim on `/economics`. Its ranked
/// rows (`rules: anthropos`) are verified Tip5 hashes per second — the unit
/// network difficulty counts since height 147,500 — and carry an estimate.
/// Its pre-Anthropos rows measure complete %3 proofs per second; dividing
/// those by a difficulty in hashes gives a number (≈ 0 NOCK/day, which the
/// board printed for weeks) that means nothing, so theirs is omitted, not
/// zeroed.
pub const ZK_PROOF_ROWS_UNPRICED: &str = "ZK v5 rows (rules anthropos) are verified Tip5 hashes/s, the unit difficulty counts since height 147,500, and are priced from that ranked rate; pre-anthropos rows are complete %3 proofs/sec, which does not price, so they carry no estimate";

/// ZK track: `attempts_per_sec × 86400 / difficulty × block_reward_nock`.
/// Since Anthropos the argument must be Tip5 hashes per second.
pub fn nock_per_day(attempts_per_sec: f64, p: &EconParams) -> f64 {
    attempts_per_sec * 86_400.0 / p.difficulty * p.block_reward_nock
}

/// AI track: `mac_per_sec × 86400 / (2^256/(T_ai+1)) × block_reward_nock`,
/// i.e. the same attempt lottery counted in MAC-equivalents instead of proof
/// attempts. `None` until an `ai-pow` tip has been observed — there is no
/// honest fallback, and a zero would read as "this machine earns nothing"
/// rather than "we do not know".
///
/// The caller must pass a RANKED rate (`verified_mac_per_sec_lb`), never a
/// client-reported grind rate: this function turns its argument into money,
/// and the only rate this registry stands behind is the one it measured
/// itself.
pub fn ai_nock_per_day(mac_per_sec: f64, p: &EconParams) -> Option<f64> {
    let ai = p.ai.as_ref()?;
    Some(mac_per_sec * 86_400.0 / ai.mac_equiv_per_block * p.block_reward_nock)
}

/// `nock_per_day × spot price`, or `None` when no price has been observed.
///
/// Deliberately takes the ALREADY-PUBLISHED NOCK/day figure rather than
/// recomputing from the rate: a reader can then check every USD number on the
/// board with one multiplication against two numbers we also print.
pub fn usd_per_day(nock_per_day: f64, p: &EconParams) -> Option<f64> {
    Some(signif4(nock_per_day * p.price.as_ref()?.usd))
}

/// Estimated whole-network ZK attempt rate (Tip5 hashes/sec since
/// Anthropos): difficulty is expected attempts per block, and ZK blocks
/// arrive every block_time_secs() on average.
pub fn network_pps(p: &EconParams) -> f64 {
    p.difficulty / block_time_secs()
}

/// The AI analogue: expected MAC-equivalents per block over the AI block
/// interval. `None` until an AI tip has been observed.
pub fn ai_network_mac_per_sec(p: &EconParams) -> Option<f64> {
    Some(p.ai.as_ref()?.mac_equiv_per_block / ai_block_time_secs())
}

/// Round to 4 significant digits.
///
/// The figures this module and [`crate::aipow`] print span ~1e-6 (USD/day for
/// one CPU) to ~1e11 (MAC/s for a GPU), so fixed decimal places would be
/// either lossy at one end or noisy at the other. One helper, one home: the
/// AI rates import it from here.
pub fn signif4(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let scale = 10f64.powf(3.0 - x.abs().log10().floor());
    (x * scale).round() / scale
}

/// The operator-supplied seed: ZK difficulty and block reward, nothing
/// observed yet. Everything else in [`EconParams`] is learned at runtime.
pub fn from_env() -> Option<EconParams> {
    let difficulty: f64 = std::env::var("NOCKMARK_DIFFICULTY").ok()?.parse().ok()?;
    let block_reward_nock: f64 =
        std::env::var("NOCKMARK_BLOCK_REWARD_NOCK").ok()?.parse().ok()?;
    (difficulty > 0.0 && block_reward_nock > 0.0)
        .then(|| EconParams::seed(difficulty, block_reward_nock))
}

impl EconParams {
    /// A params block carrying only the two operator-supplied ZK figures —
    /// no observation times, no AI difficulty, no price. This is the state
    /// every instance boots in, and the state a price-less instance stays in.
    pub fn seed(difficulty: f64, block_reward_nock: f64) -> Self {
        Self {
            difficulty,
            difficulty_source: None,
            block_reward_nock,
            block_reward_observed_at: None,
            difficulty_observed_at: None,
            difficulty_tip_timestamp: None,
            ai: None,
            price: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Price feed
// ---------------------------------------------------------------------------

/// Default NOCK/USD source: CoinGecko's free simple-price endpoint (no key,
/// no account). Cross-checked 2026-08-22 against nock.dwd.com's own
/// `Price_USD`, which sat 0.3% away — well inside the spread a spot price
/// carries anyway — so CoinGecko is the primary and the disagreement is not
/// worth a second feed.
pub const DEFAULT_PRICE_URL: &str =
    "https://api.coingecko.com/api/v3/simple/price?ids=nockchain&vs_currencies=usd";

/// The instance's price source: NOCKMARK_PRICE_URL if set, else
/// [`DEFAULT_PRICE_URL`]. Setting it empty (or to `off`) disables the feed
/// entirely, which is a supported configuration and not a degraded one — an
/// instance that publishes no USD figure is making no claim about USD.
pub fn price_url() -> Option<String> {
    match std::env::var("NOCKMARK_PRICE_URL") {
        Ok(s) if s.trim().is_empty() || s.trim() == "off" => None,
        Ok(s) => Some(s),
        Err(_) => Some(DEFAULT_PRICE_URL.to_string()),
    }
}

/// Descriptive User-Agent on every outbound request.
///
/// Not optional politeness. CoinGecko's free endpoint answers 403 with
/// "Please add a descriptive User-Agent to your request" when there is none,
/// and reqwest sends none by default — the first live run of this feed failed
/// on exactly that, and reported it as a missing `"usd"` field, which is why
/// [`json_body`] now reads the status first.
const USER_AGENT: &str = concat!(
    "nockmark-registry/",
    env!("CARGO_PKG_VERSION"),
    " (+https://nockmark.xyz)"
);

/// Read a JSON body, turning a non-2xx into an error that says what the
/// server actually said.
///
/// Without this, an HTTP error whose body happens to be valid JSON reaches
/// the field extractors and comes back as "no numeric X field" — an error
/// that sends you looking in exactly the wrong place.
async fn json_body(resp: reqwest::Response, url: &str) -> Result<serde_json::Value, String> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("reading body from {url}: {e}"))?;
    if !status.is_success() {
        let snippet: String = text.chars().take(300).collect();
        return Err(format!("{url} returned HTTP {status}: {snippet}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("bad JSON from {url}: {e}"))
}

/// GET `url` and read a USD price out of it.
async fn fetch_price(client: &reqwest::Client, url: &str) -> Result<f64, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let v = json_body(resp, url).await?;
    let usd = usd_field(&v).ok_or_else(|| format!("no numeric \"usd\" field at {url}"))?;
    // A zero, negative or absurd price is worse than no price: it would turn
    // every USD figure on the board into a confident lie. Reject and keep
    // whatever we had, exactly as the difficulty path does.
    if !(usd > 0.0 && usd < 1e6) {
        return Err(format!("implausible NOCK price {usd} from {url}"));
    }
    Ok(usd)
}

/// Find a numeric `"usd"` value anywhere in the document, depth-first.
///
/// CoinGecko nests it one level under the coin id
/// (`{"nockchain":{"usd":0.0218}}`), and an operator pointing
/// NOCKMARK_PRICE_URL at some other quote endpoint will nest it somewhere
/// else. Walking for the key is the same leniency [`difficulty_field`] already
/// applies to `"difficulty"`, and it keeps the override from needing a code
/// change to be useful.
fn usd_field(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                if k.eq_ignore_ascii_case("usd") {
                    if let Some(x) = val.as_f64() {
                        return Some(x);
                    }
                }
            }
            map.values().find_map(usd_field)
        }
        serde_json::Value::Array(xs) => xs.iter().find_map(usd_field),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Refresh loop
// ---------------------------------------------------------------------------

/// Which track an observed tip belongs to, and what it is worth.
///
/// The two are kept strictly apart. An AI target read as a ZK difficulty (or
/// the reverse) is not a small error: the two live ~2^59 apart in the SAME
/// `target` field of the same `$page` (the puzzle tag lives in the pow
/// artifact, not beside the target), so a mix-up produces a number that is
/// wrong by orders of magnitude and looks like a number.
enum Tip {
    Zk {
        /// Expected attempts per block (Tip5 hashes since Anthropos).
        difficulty: f64,
        /// The block's own timestamp, if the source reports one.
        tip_timestamp: Option<u64>,
    },
    Ai {
        /// Expected MAC-equivalents per block.
        mac_equiv_per_block: f64,
        /// The raw target, verbatim, when the source gives one.
        target: Option<String>,
        tip_timestamp: Option<u64>,
    },
    /// The whole block reward in NOCK, BEFORE the 80/20 split — what
    /// explorers publish. [`MINER_SHARE`] is applied when it is folded in.
    Reward { total_nock: f64 },
}

/// The miner's share of each block's emission: `+new-with-fund-share` pays
/// `emission / 5` to the protocol fund (tx-engine-1.hoon), the rest to the
/// miner. Checked against the chain: block 181,396's coinbase is 80,530,637 +
/// 20,132,659 nicks, and 80,530,637 / 65,536 nicks-per-NOCK = 1228.8 = 0.8 ×
/// 1536, the eon-4 reward.
pub const MINER_SHARE: f64 = 0.8;

/// One configured difficulty source.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// Plain GET: nockscan `mining-metrics`, or any JSON with a top-level
    /// numeric "difficulty".
    Get(String),
    /// NockBlocks-style JSON-RPC getTip with an API key.
    Rpc { url: String, api_key: String },
}

impl Source {
    pub fn url(&self) -> &str {
        match self {
            Source::Get(u) => u,
            Source::Rpc { url, .. } => url,
        }
    }
}

/// The instance's difficulty sources in priority order, from
/// NOCKMARK_ECON_URL / NOCKMARK_ECON_RPC_URL / NOCKMARK_ECON_API_KEY.
pub fn sources_from_env() -> Vec<Source> {
    let var = |k| std::env::var(k).ok().filter(|s: &String| !s.trim().is_empty());
    sources(
        var("NOCKMARK_ECON_URL"),
        var("NOCKMARK_ECON_RPC_URL"),
        var("NOCKMARK_ECON_API_KEY"),
    )
}

/// Pure half of [`sources_from_env`], so the precedence rules are testable.
///
/// The GET source is primary when there is one: nockscan needs no key, gives
/// both tracks' difficulty and the reward in one call, and its numbers are its
/// own per-puzzle aggregates rather than whichever block happens to sit at the
/// tip. The RPC source is the fallback. Before M7 the single source was
/// NOCKMARK_ECON_URL + NOCKMARK_ECON_API_KEY meaning RPC; that combination,
/// with no RPC URL set, still means exactly that.
fn sources(get: Option<String>, rpc_url: Option<String>, key: Option<String>) -> Vec<Source> {
    match (get, rpc_url, key) {
        (Some(url), None, Some(api_key)) => vec![Source::Rpc { url, api_key }],
        (get, rpc_url, key) => {
            let mut out: Vec<Source> = get.into_iter().map(Source::Get).collect();
            if let (Some(url), Some(api_key)) = (rpc_url, key) {
                out.push(Source::Rpc { url, api_key });
            }
            out
        }
    }
}

/// Poll the difficulty sources and the price feed every 10 minutes and update
/// the shared params. Only refreshes an already-configured cache. Sources are
/// tried in order and the first that answers wins the tick; a failure falls
/// through to the next. Each successful difficulty observation is appended to
/// `history` (JSONL on the persistent volume) so the board can show difficulty
/// over time.
///
/// `sources` may be empty: an instance with a hand-set difficulty can still
/// run this loop for the price alone.
pub async fn refresh_loop(
    sources: Vec<Source>,
    history: Option<std::path::PathBuf>,
    econ: Arc<RwLock<Option<EconParams>>>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(USER_AGENT)
        .build()
        .expect("reqwest client");
    let price_url = price_url();
    let mut tick = tokio::time::interval(Duration::from_secs(600));
    loop {
        tick.tick().await;
        for source in &sources {
            let fetched = match source {
                Source::Rpc { url, api_key } => {
                    fetch_tip_rpc(&client, url, api_key).await.map(|t| vec![t])
                }
                Source::Get(url) => fetch_get(&client, url).await,
            };
            match fetched {
                Ok(tips) => {
                    for tip in tips {
                        apply_tip(tip, source.url(), &econ, history.as_deref()).await;
                    }
                    break;
                }
                Err(e) => eprintln!(
                    "econ refresh from {} failed (trying the next source, else keeping cached values): {e}",
                    source.url()
                ),
            }
        }
        // The price is fetched whatever the difficulty read did: a chain
        // explorer outage must not take the USD column down with it, or the
        // reverse. Both carry their own observation time for the same reason.
        if let Some(price_url) = &price_url {
            match fetch_price(&client, price_url).await {
                Ok(usd) => {
                    if let Some(p) = econ.write().await.as_mut() {
                        p.price = Some(NockPrice {
                            usd,
                            source: price_url.clone(),
                            observed_at: unix_now(),
                        });
                    }
                }
                Err(e) => eprintln!("price refresh failed (keeping cached value): {e}"),
            }
        }
    }
}

/// Fold one tip into the cache. Each track's difficulty is written only by a
/// tip of its own type and carries the time IT was observed — so a day
/// without an AI block leaves the AI figure visibly stale rather than
/// silently absent or, worse, silently ZK-derived.
async fn apply_tip(
    tip: Tip,
    source: &str,
    econ: &Arc<RwLock<Option<EconParams>>>,
    history: Option<&std::path::Path>,
) {
    let now = unix_now();
    let mut guard = econ.write().await;
    // Nothing to refresh on an instance that was never configured: without a
    // seeded reward an RPC-only instance would hold half a model.
    let Some(p) = guard.as_mut() else {
        return;
    };
    let (kind, value) = match tip {
        Tip::Reward { total_nock } => {
            // Rounded to 1e-4 NOCK: 1536 × 0.8 is 1228.8000000000002 in f64,
            // which /economics and the board would print verbatim. The chain
            // itself pays whole nicks (80,530,637 = 1228.80000305 NOCK), so
            // four decimals is already past what the figure can claim.
            let miner = (total_nock * MINER_SHARE * 1e4).round() / 1e4;
            if p.block_reward_nock != miner {
                eprintln!(
                    "econ refresh: block reward {} -> {miner} NOCK (miner share of {total_nock})",
                    p.block_reward_nock
                );
            }
            p.block_reward_nock = miner;
            p.block_reward_observed_at = Some(now);
            // Steps once per eon: not worth a history line every tick.
            return;
        }
        Tip::Zk {
            difficulty,
            tip_timestamp,
        } => {
            if p.difficulty != difficulty {
                eprintln!("econ refresh: difficulty {} -> {difficulty}", p.difficulty);
            }
            p.difficulty = difficulty;
            p.difficulty_source = Some(source.to_string());
            p.difficulty_observed_at = Some(now);
            p.difficulty_tip_timestamp = tip_timestamp;
            ("difficulty", difficulty)
        }
        Tip::Ai {
            mac_equiv_per_block,
            target,
            tip_timestamp,
        } => {
            let previous = p.ai.as_ref().map(|a| a.mac_equiv_per_block);
            if previous != Some(mac_equiv_per_block) {
                eprintln!(
                    "econ refresh: ai difficulty {} -> {mac_equiv_per_block} MAC-equiv/block",
                    previous.map_or_else(|| "unseen".to_string(), |d| d.to_string())
                );
            }
            p.ai = Some(AiDifficulty {
                mac_equiv_per_block,
                target,
                source: source.to_string(),
                observed_at: now,
                tip_timestamp,
            });
            ("ai_difficulty", mac_equiv_per_block)
        }
    };
    drop(guard);
    if let Some(path) = history {
        if let Err(e) = append_observation(path, now, kind, value) {
            eprintln!("econ refresh: history append failed: {e}");
        }
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How long ago `observed_at` was, in seconds. Saturating: a clock that has
/// stepped backwards should read "just now", not underflow to an age of half
/// a billion years.
pub fn age_secs(observed_at: u64) -> u64 {
    unix_now().saturating_sub(observed_at)
}

/// One observation, `{"t":unix_secs,"<kind>":n}` per line, where `<kind>` is
/// `difficulty` (ZK) or `ai_difficulty`. At one line per 10-minute poll this
/// grows ~7 KB/day — no rotation needed on any horizon that matters.
///
/// The two kinds share a file but not a key, which is what keeps
/// [`read_history`] — and the board's difficulty sparkline reading it —
/// blind to AI lines without needing to know they exist.
fn append_observation(
    path: &std::path::Path,
    t: u64,
    kind: &str,
    value: f64,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::json!({ "t": t, kind: value }))
}

/// Read ZK difficulty observations newer than `since` (unix secs), oldest
/// first, thinned to at most `cap` evenly-spaced points. Unparseable lines
/// (torn writes) and lines of another kind are skipped.
pub fn read_history(
    path: &std::path::Path,
    since: u64,
    cap: usize,
) -> std::io::Result<Vec<(u64, f64)>> {
    let raw = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        r => r?,
    };
    let pts: Vec<(u64, f64)> = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| Some((v["t"].as_u64()?, v["difficulty"].as_f64()?)))
        .filter(|(t, _)| *t >= since)
        .collect();
    if pts.len() <= cap || cap == 0 {
        return Ok(pts);
    }
    // Evenly-spaced thinning that always keeps the newest point.
    let last = *pts.last().unwrap();
    let step = pts.len() as f64 / (cap - 1) as f64;
    let mut out: Vec<(u64, f64)> =
        (0..cap - 1).map(|i| pts[(i as f64 * step) as usize]).collect();
    out.push(last);
    Ok(out)
}

/// GET source. nockscan's `mining-metrics` is recognised by its
/// `log2Difficulty` field and read in full ([`tips_from_nockscan`]);
/// anything else must carry a top-level numeric "difficulty"
/// (case-insensitive key — nock.dwd.com spelled it "Difficulty"), which says
/// nothing about pow type and so can only ever teach us the ZK figure.
async fn fetch_get(client: &reqwest::Client, url: &str) -> Result<Vec<Tip>, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let v = json_body(resp, url).await?;
    if v.get("log2Difficulty").is_some() {
        return tips_from_nockscan(&v, url);
    }
    let difficulty =
        difficulty_field(&v).ok_or_else(|| format!("no numeric \"difficulty\" field at {url}"))?;
    if !(difficulty > 0.0) {
        return Err(format!("non-positive difficulty {difficulty} at {url}"));
    }
    // No block behind this shape, so no chain time to report.
    Ok(vec![Tip::Zk {
        difficulty,
        tip_timestamp: None,
    }])
}

/// Largest ZK difficulty a legal target can produce: `p^5 / 2^232` ≈ 2^88.
/// A ZK target below the AI cap would be an AI target mislabelled (see
/// [`difficulty_from_target`]); here the same band bounds a published
/// difficulty instead of a target.
const MAX_ZK_LOG2_DIFFICULTY: f64 = 88.0;
/// Smallest AI difficulty a legal target can produce: `2^256 / 2^232` = 2^24
/// MAC-equivalents per block (consensus caps AI targets below 2^232).
const MIN_AI_LOG2_DIFFICULTY: f64 = 24.0;

/// Read nockscan `/api/v1/mining-metrics` (checked against the live API on
/// 2026-11-11): `log2Difficulty` is log2 of expected ZK attempts per block,
/// `aiLog2Difficulty` log2 of expected MAC-equivalents per AI block,
/// `blockRewardNock` the whole reward before the 80/20 split, and
/// `latestBlockTimestamp` the timestamp of the tip, whose puzzle `tipPuzzle`
/// names — so only that track's figure is pinned to a block time.
///
/// Unlike the RPC tip, every figure here sits in its own named field, so the
/// track mix-up [`tip_from_rpc_result`] guards against by range cannot happen
/// by construction; the ranges are still checked, against garbage.
///
/// Also cross-checks the chain's own exchange rate,
/// `workNormalization.macsPerZkUnit`, against [`MAC_EQUIV_PER_ZK_ATTEMPT`]. A
/// mismatch means a fork changed the rate and this build has not caught up:
/// the difficulties are still right, so they are kept, but it is shouted.
fn tips_from_nockscan(v: &serde_json::Value, url: &str) -> Result<Vec<Tip>, String> {
    let zk_log2 = v["log2Difficulty"]
        .as_f64()
        .ok_or_else(|| format!("non-numeric \"log2Difficulty\" at {url}"))?;
    if !(zk_log2 > 0.0 && zk_log2 < MAX_ZK_LOG2_DIFFICULTY) {
        return Err(format!("zk log2 difficulty {zk_log2} outside (0, 88) at {url}"));
    }
    let tip_timestamp = v["latestBlockTimestamp"].as_u64();
    let tip_puzzle = v["tipPuzzle"].as_str();
    let mut tips = vec![Tip::Zk {
        difficulty: zk_log2.exp2(),
        tip_timestamp: tip_timestamp.filter(|_| tip_puzzle == Some("zk")),
    }];
    if let Some(ai_log2) = v["aiLog2Difficulty"].as_f64() {
        if !(ai_log2 > MIN_AI_LOG2_DIFFICULTY && ai_log2 < 256.0) {
            return Err(format!("ai log2 difficulty {ai_log2} outside (24, 256) at {url}"));
        }
        tips.push(Tip::Ai {
            mac_equiv_per_block: ai_log2.exp2(),
            target: None,
            tip_timestamp: tip_timestamp.filter(|_| tip_puzzle == Some("ai")),
        });
    }
    // The reward is optional: a missing or implausible one leaves the cached
    // reward alone rather than failing the difficulty read with it. Every eon
    // of the schedule pays between 64 (the tail) and 65,536 NOCK.
    if let Some(total) = v["blockRewardNock"].as_f64().filter(|r| (64.0..=65_536.0).contains(r)) {
        tips.push(Tip::Reward { total_nock: total });
    }
    if let Some(rate) = v["workNormalization"]["macsPerZkUnit"].as_f64() {
        if rate != MAC_EQUIV_PER_ZK_ATTEMPT {
            eprintln!(
                "econ refresh: chain exchange rate is {rate} MAC per ZK attempt but this build \
                 assumes {MAC_EQUIV_PER_ZK_ATTEMPT} — a fork has moved it; update the constant"
            );
        }
    }
    Ok(tips)
}

fn difficulty_field(v: &serde_json::Value) -> Option<f64> {
    v.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("difficulty"))?
        .1
        .as_f64()
}

/// NockBlocks-style source: POST a getTip JSON-RPC call with the API key
/// and price that tip's target under its own track's rule.
async fn fetch_tip_rpc(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
) -> Result<Tip, String> {
    let resp = client
        .post(url)
        .header("x-api-key", api_key)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "method": "getTip", "params": [], "id": "nockmark-econ",
        }))
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let v = json_body(resp, url).await?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(format!("RPC error from {url}: {err}"));
    }
    tip_from_rpc_result(&v["result"], url)
}

/// Split out from the transport so the powType→formula mapping — the one
/// place a mix-up would be both catastrophic and invisible — is directly
/// testable.
///
/// `powType` is NOT a nockchain interface: nothing in the protocol repo
/// defines the name or the `"zk-pow"`/`"ai-pow"` values, and the node's own
/// gRPC `BlockDetails` carries no puzzle discriminator at all. It is an
/// explorer convention, so it is treated as a hint that must survive a
/// range check, never as a fact. That is what makes the missing-field case
/// safe: the two tracks' targets cannot pass each other's band.
fn tip_from_rpc_result(result: &serde_json::Value, url: &str) -> Result<Tip, String> {
    let target = result["target"]
        .as_str()
        .ok_or_else(|| format!("no string \"result.target\" field at {url}"))?;
    // The block's own unix timestamp. Kept because it, and not our polling
    // clock, is what says whether the chain is actually moving.
    let tip_timestamp = result["timestamp"].as_u64();
    match result["powType"].as_str() {
        // Pre-Logos responses have no powType field and are ZK by
        // construction; post-Logos, a dropped or renamed field lands here too,
        // and `difficulty_from_target`'s floor rejects an AI target outright.
        None | Some("zk-pow") => Ok(Tip::Zk {
            difficulty: difficulty_from_target(target)?,
            tip_timestamp,
        }),
        Some("ai-pow") => Ok(Tip::Ai {
            mac_equiv_per_block: ai_difficulty_from_target(target)?,
            target: Some(target.to_string()),
            tip_timestamp,
        }),
        // A third pow type is not something to guess at: a target we cannot
        // price is dropped, and both cached figures survive untouched.
        Some(other) => Err(format!("tip is a {other} block — no formula for it")),
    }
}

/// ZK: expected proof attempts per block, `p^5 / (T + 1)`.
///
/// The floor is [`MAX_AI_TARGET_ATOM`], not zero. A ZK target sits near 2^291
/// (the post-Logos anchor is `375 · 2^291 / 214`) and consensus caps AI
/// targets at 2^232, so nothing legitimate is excluded — but an AI target
/// mislabelled as ZK is, instead of silently becoming a difficulty ~2^59
/// times too large.
fn difficulty_from_target(target: &str) -> Result<f64, String> {
    let t: f64 = target
        .parse()
        .map_err(|e| format!("unparseable target {target:?}: {e}"))?;
    if !(t > MAX_AI_TARGET_ATOM && t < MAX_TIP5_ATOM) {
        return Err(format!("zk target {target} outside (2^232, p^5)"));
    }
    Ok(MAX_TIP5_ATOM / (t + 1.0))
}

/// AI: expected MAC-equivalents per block, `2^256 / (T_ai + 1)` — the Rust
/// port of `+ai-pow-work` (tx-engine.hoon), minus its conversion into
/// ZK-attempt-equivalents.
///
/// Note the unit. This is NOT "expected attempts", because an AI attempt has
/// no fixed size: consensus compares the jackpot against `Θ = T · F` for a
/// shape worth `F` MAC-equivalents per attempt, so the attempt count is
/// `2^256/(Θ+1)` and the MAC-equivalent count is `F ×` that — the `F`s cancel
/// and what is left is shape-independent (invariant D2). That cancellation is
/// what lets a dense row and a canonical-MoE row be priced by one number.
fn ai_difficulty_from_target(target: &str) -> Result<f64, String> {
    let t: f64 = target
        .parse()
        .map_err(|e| format!("unparseable ai target {target:?}: {e}"))?;
    if !(t > 0.0 && t < MAX_AI_TARGET_ATOM) {
        return Err(format!("ai target {target} outside (0, 2^232)"));
    }
    Ok(TWO_POW_256 / (t + 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(difficulty: f64, block_reward_nock: f64) -> EconParams {
        EconParams::seed(difficulty, block_reward_nock)
    }

    /// The Logos AI ASERT anchor as a decimal string: 2^192.
    const AI_ANCHOR_TARGET: &str = "6277101735386680763835789423207666416102355444464034512896";

    /// A real post-Logos ZK tip target (block 126858, 2026-08-14).
    const ZK_TIP_TARGET: &str = "842169517667840109699944519496662570701369765334199539280992969208055405254838989986691";

    #[test]
    fn whole_network_rate_earns_the_whole_zk_emission() {
        // 500 s ZK blocks (Anthropos) → 172.8/day. A miner who IS the entire
        // ZK network (rate × 500 s = difficulty) earns exactly 172.8 × reward
        // per day — the ~30% of daily emission ZK blocks carry.
        let p = params(6_000.0, 1_228.8);
        let est = nock_per_day(network_pps(&p), &p);
        assert!((est - 172.8 * p.block_reward_nock).abs() < 1e-6);
    }

    #[test]
    fn est_scales_linearly_with_pps() {
        let p = params(1_000_000.0, 2_048.0);
        assert!((nock_per_day(0.1, &p) * 2.0 - nock_per_day(0.2, &p)).abs() < 1e-9);
    }

    #[test]
    fn from_env_requires_both_vars() {
        // (Runs single-threaded within this test fn; env is process-global,
        // so restore it.)
        std::env::remove_var("NOCKMARK_DIFFICULTY");
        std::env::remove_var("NOCKMARK_BLOCK_REWARD_NOCK");
        assert!(from_env().is_none());
        std::env::set_var("NOCKMARK_DIFFICULTY", "5000000");
        assert!(from_env().is_none(), "difficulty alone is not enough");
        std::env::set_var("NOCKMARK_BLOCK_REWARD_NOCK", "2048");
        let p = from_env().unwrap();
        assert_eq!(p.difficulty, 5_000_000.0);
        assert_eq!(p.block_reward_nock, 2_048.0);
        // A seed is not an observation: nothing is timestamped, and neither
        // the AI difficulty nor the price is invented.
        assert_eq!(p.difficulty_observed_at, None);
        assert!(p.ai.is_none() && p.price.is_none());
        std::env::remove_var("NOCKMARK_DIFFICULTY");
        std::env::remove_var("NOCKMARK_BLOCK_REWARD_NOCK");
    }

    #[test]
    fn difficulty_field_is_case_insensitive() {
        let lower: serde_json::Value = serde_json::json!({"difficulty": 12.5});
        let upper: serde_json::Value = serde_json::json!({"Difficulty": 1073741824.0, "x": 1});
        assert_eq!(difficulty_field(&lower), Some(12.5));
        assert_eq!(difficulty_field(&upper), Some(1_073_741_824.0));
        assert_eq!(difficulty_field(&serde_json::json!({"diff": 1})), None);
        assert_eq!(difficulty_field(&serde_json::json!({"difficulty": "n/a"})), None);
    }

    #[test]
    fn difficulty_from_real_mainnet_target() {
        // Block 124570 (2026-08-11): nockblocks getTip returned this target,
        // and the chain's own accumulatedWork delta over the parent block —
        // i.e. this block's work, computed by consensus in exact bignum
        // arithmetic — was 2_491_784_163.
        let target = "857211898323310214279691199033669776696186506919492829732100799763647239070693679512270";
        let d = difficulty_from_target(target).unwrap();
        assert!((d - 2_491_784_163.0).abs() / 2_491_784_163.0 < 1e-9, "got {d}");
    }

    /// **The AI formula, checked against the protocol's own anchor.**
    ///
    /// Logos anchors AI ASERT at `T_ai = 2^192` (`anchor-target-atom` of
    /// `+$ ai-asert`), and `changelog/protocol/016-logos.md` states that the
    /// anchor block's expected work is `2^256 / anchor = 2^64`
    /// MAC-equivalents. If `2^256/(T+1)` did not reproduce that, every AI
    /// earnings figure on the board would be wrong by whatever factor the
    /// mistake was — and nothing else in the system would notice, because
    /// there is no second source for AI difficulty to disagree with.
    #[test]
    fn ai_difficulty_reproduces_the_logos_asert_anchor() {
        assert_eq!(
            AI_ANCHOR_TARGET.parse::<f64>().unwrap(),
            2f64.powi(192),
            "the anchor literal really is 2^192"
        );
        let d = ai_difficulty_from_target(AI_ANCHOR_TARGET).unwrap();
        assert_eq!(d, 2f64.powi(64), "2^256/(2^192+1) = 2^64 MAC-equiv/block");

        // …and the earnings formula built on it: at the anchor, a machine
        // doing 2^64 MAC-equivalents/sec would expect one block per second,
        // i.e. 86400 blocks/day.
        let mut p = params(1.0, 1.0);
        p.ai = Some(AiDifficulty {
            mac_equiv_per_block: d,
            target: Some(AI_ANCHOR_TARGET.to_string()),
            source: "test".into(),
            observed_at: 0,
            tip_timestamp: None,
        });
        assert_eq!(ai_nock_per_day(2f64.powi(64), &p).unwrap(), 86_400.0);

        // The Logos changelog put the same anchor at ~3.7e16 MAC/s for the
        // whole AI network at its 500 s ideal. Anthropos moved the AI ideal to
        // 214 s, so the same difficulty now reads 2^64 / 214 ≈ 8.62e16.
        assert!(
            (ai_network_mac_per_sec(&p).unwrap() - 8.62e16).abs() < 0.01e16,
            "{:?}",
            ai_network_mac_per_sec(&p)
        );
    }

    /// A worked AI example with round numbers, and linearity in the rate.
    #[test]
    fn ai_earnings_worked_example_and_linearity() {
        // 2 MAC-equivalents per block — an absurd difficulty, chosen so the
        // arithmetic is checkable by hand.
        let mut p = params(1.0, 1_000.0);
        p.ai = Some(AiDifficulty {
            mac_equiv_per_block: 2.0,
            target: None,
            source: "test".into(),
            observed_at: 0,
            tip_timestamp: None,
        });
        // 10 MAC-equiv/s × 86400 s ÷ 2 per block = 432_000 blocks/day
        // × 1000 NOCK = 432_000_000 NOCK/day.
        assert_eq!(ai_nock_per_day(10.0, &p).unwrap(), 432_000_000.0);
        assert_eq!(ai_nock_per_day(20.0, &p).unwrap(), 864_000_000.0);
        assert_eq!(
            ai_nock_per_day(1.0, &p).unwrap() * 7.5,
            ai_nock_per_day(7.5, &p).unwrap(),
            "linear in the rate"
        );
        // And with a price attached, USD is that times the price.
        p.price = Some(NockPrice {
            usd: 0.01,
            source: "test".into(),
            observed_at: 0,
        });
        let nock = ai_nock_per_day(10.0, &p).unwrap();
        assert_eq!(usd_per_day(nock, &p), Some(4_320_000.0));
    }

    /// Until an AI tip is seen there is no AI figure at all — not a zero.
    #[test]
    fn ai_earnings_are_absent_before_an_ai_tip() {
        let p = params(6_000.0, 2_048.0);
        assert!(p.ai.is_none());
        assert_eq!(ai_nock_per_day(1e11, &p), None);
        assert_eq!(ai_network_mac_per_sec(&p), None);
        // …while the ZK side is entirely unaffected.
        assert!(nock_per_day(0.2, &p) > 0.0);
    }

    /// Same rule for USD: no price, no figure.
    #[test]
    fn usd_is_absent_without_a_price_and_multiplies_when_present() {
        let mut p = params(6_000.0, 2_048.0);
        assert_eq!(usd_per_day(1.0, &p), None);
        p.price = Some(NockPrice {
            usd: 0.02,
            source: "test".into(),
            observed_at: 0,
        });
        assert_eq!(usd_per_day(1.5, &p), Some(0.03));
        // Zero NOCK/day is a real zero, not a missing figure.
        assert_eq!(usd_per_day(0.0, &p), Some(0.0));
    }

    /// A ZK tip and an AI tip are read by different formulas into different
    /// slots, and neither target can be read under the other's rule. This is
    /// the single most damaging mistake available here, so it is tested
    /// directly rather than left to the integration path.
    #[test]
    fn zk_and_ai_tips_cannot_be_read_as_each_other() {
        // The real block-126858 response, fields and all.
        let zk = serde_json::json!({
            "powType": "zk-pow", "target": ZK_TIP_TARGET, "timestamp": 1_786_724_338u64,
        });
        match tip_from_rpc_result(&zk, "test").unwrap() {
            Tip::Zk {
                difficulty,
                tip_timestamp,
            } => {
                assert!((difficulty - 2_536_291_077.536_658_3).abs() < 1.0, "{difficulty}");
                assert_eq!(tip_timestamp, Some(1_786_724_338), "chain time, not our time");
            }
            _ => panic!("zk-pow tip must read as ZK"),
        }

        let ai = serde_json::json!({ "powType": "ai-pow", "target": AI_ANCHOR_TARGET });
        match tip_from_rpc_result(&ai, "test").unwrap() {
            Tip::Ai {
                mac_equiv_per_block,
                target,
                tip_timestamp,
            } => {
                assert_eq!(mac_equiv_per_block, 2f64.powi(64));
                assert_eq!(target.as_deref(), Some(AI_ANCHOR_TARGET), "the raw target is kept verbatim");
                assert_eq!(tip_timestamp, None, "a source that omits it is not invented");
            }
            _ => panic!("ai-pow tip must read as AI"),
        }

        // Pre-Logos tips carry no powType and are ZK by construction.
        assert!(matches!(
            tip_from_rpc_result(&serde_json::json!({ "target": ZK_TIP_TARGET }), "test").unwrap(),
            Tip::Zk { .. }
        ));
        // …and if the explorer ever DROPPED powType on an AI tip, the band
        // fails closed instead of publishing a difficulty 2^59 times too big.
        assert!(
            tip_from_rpc_result(&serde_json::json!({ "target": AI_ANCHOR_TARGET }), "test")
                .is_err(),
            "an AI-scale target must never be priced as ZK"
        );
        // The reverse mislabelling is refused just as flatly.
        assert!(ai_difficulty_from_target(ZK_TIP_TARGET).is_err());

        // An unknown third pow type is dropped, not guessed at.
        let other = serde_json::json!({ "powType": "quantum-pow", "target": ZK_TIP_TARGET });
        assert!(tip_from_rpc_result(&other, "test").is_err());
    }

    /// Each track's difficulty is refreshed only by a tip of its own type,
    /// and each keeps its own observed-at — the property that makes a stale
    /// AI figure visibly stale instead of quietly wrong.
    #[tokio::test]
    async fn each_track_keeps_its_own_observation_time() {
        let econ = Arc::new(RwLock::new(Some(params(1_000.0, 2_048.0))));
        apply_tip(
            Tip::Ai {
                mac_equiv_per_block: 2f64.powi(64),
                target: Some(AI_ANCHOR_TARGET.to_string()),
                tip_timestamp: Some(1_786_000_000),
            },
            "rpc-source",
            &econ,
            None,
        )
        .await;
        let p = econ.read().await.clone().unwrap();
        // The AI tip taught us the AI difficulty…
        assert_eq!(p.ai.as_ref().unwrap().mac_equiv_per_block, 2f64.powi(64));
        assert!(p.ai.as_ref().unwrap().observed_at > 0);
        assert_eq!(p.ai.as_ref().unwrap().tip_timestamp, Some(1_786_000_000));
        // …and left the ZK difficulty exactly as it was, still unobserved.
        assert_eq!(p.difficulty, 1_000.0);
        assert_eq!(p.difficulty_observed_at, None);
        assert_eq!(p.difficulty_tip_timestamp, None);

        apply_tip(
            Tip::Zk {
                difficulty: 2_000.0,
                tip_timestamp: Some(1_786_724_338),
            },
            "get-source",
            &econ,
            None,
        )
        .await;
        let p = econ.read().await.clone().unwrap();
        assert_eq!(p.difficulty, 2_000.0);
        assert!(p.difficulty_observed_at.unwrap() > 0);
        // We fetched it just now, but the block it came from is old — the
        // distinction the two timestamps exist to make.
        assert_eq!(p.difficulty_tip_timestamp, Some(1_786_724_338));
        assert!(
            p.difficulty_observed_at.unwrap() - p.difficulty_tip_timestamp.unwrap() > 86_400,
            "observed-now, mined-days-ago must be distinguishable"
        );
        // The AI figure is untouched by a ZK tip — including its timestamps.
        assert_eq!(p.ai.as_ref().unwrap().mac_equiv_per_block, 2f64.powi(64));
        assert_eq!(p.ai.as_ref().unwrap().target.as_deref(), Some(AI_ANCHOR_TARGET));
        // Each figure names the source that produced it.
        assert_eq!(p.difficulty_source.as_deref(), Some("get-source"));
        assert_eq!(p.ai.as_ref().unwrap().source, "rpc-source");
        assert_eq!(p.ai.as_ref().unwrap().tip_timestamp, Some(1_786_000_000));
    }

    #[test]
    fn network_pps_is_difficulty_per_block_time() {
        // block_time_secs()/ai_block_time_secs() are 500/214 (Anthropos)
        // unless the env overrides are set, which the test env never does.
        let mut p = params(5_000.0, 1_228.8);
        assert!((network_pps(&p) - 10.0).abs() < 1e-12);
        p.ai = Some(AiDifficulty {
            mac_equiv_per_block: 2_140.0,
            target: None,
            source: "test".into(),
            observed_at: 0,
            tip_timestamp: None,
        });
        assert!((ai_network_mac_per_sec(&p).unwrap() - 10.0).abs() < 1e-12);
    }

    #[test]
    fn history_roundtrip_filters_and_thins() {
        let path = std::env::temp_dir()
            .join(format!("nockmark-econ-history-test-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_history(&path, 0, 100).unwrap(), vec![], "missing file is empty");
        for i in 0..10u64 {
            append_observation(&path, 1000 + i, "difficulty", i as f64).unwrap();
        }
        // Torn/garbage line is skipped, not fatal.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"t\": 99\n"))
            .unwrap();
        // An AI observation shares the file but not the key, so the ZK
        // sparkline never sees it.
        append_observation(&path, 1020, "ai_difficulty", 1.8e19).unwrap();
        let all = read_history(&path, 0, 100).unwrap();
        assert_eq!(all.len(), 10, "AI lines are not ZK difficulty points");
        assert_eq!(all[0], (1000, 0.0));
        let since = read_history(&path, 1005, 100).unwrap();
        assert_eq!(since.len(), 5, "since-filter keeps t >= since");
        let thinned = read_history(&path, 0, 4).unwrap();
        assert_eq!(thinned.len(), 4);
        assert_eq!(thinned[0], (1000, 0.0), "thinning keeps the oldest point");
        assert_eq!(*thinned.last().unwrap(), (1009, 9.0), "and the newest");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn targets_outside_their_bands_are_rejected() {
        assert!(difficulty_from_target("not-a-number").is_err());
        assert!(difficulty_from_target("0").is_err());
        assert!(difficulty_from_target("-5").is_err());
        // A target at/above p^5 would mean difficulty < 1 — malformed input.
        assert!(difficulty_from_target(&format!("{:.0}", MAX_TIP5_ATOM * 2.0)).is_err());
        // Same guards on the AI side, at its own consensus ceiling.
        assert!(ai_difficulty_from_target("not-a-number").is_err());
        assert!(ai_difficulty_from_target("0").is_err());
        assert!(ai_difficulty_from_target(&format!("{:.0}", MAX_AI_TARGET_ATOM)).is_err());
        // Just inside 2^232 is fine, and prices at the minimum AI difficulty
        // consensus allows: 2^256/2^232 = 2^24 MAC-equivalents per block.
        let just_inside = format!("{:.0}", MAX_AI_TARGET_ATOM * 0.999);
        let d = ai_difficulty_from_target(&just_inside).unwrap();
        assert!((d - 2f64.powi(24)).abs() / 2f64.powi(24) < 0.01, "{d}");
    }

    #[test]
    fn the_power_of_two_constants_are_exact() {
        assert_eq!(TWO_POW_256, 2f64.powi(256));
        assert_eq!(MAX_AI_TARGET_ATOM, 2f64.powi(232));
    }

    #[test]
    fn usd_field_finds_a_nested_price_and_rejects_a_non_number() {
        // The exact CoinGecko shape.
        let cg = serde_json::json!({"nockchain": {"usd": 0.02189269}});
        assert_eq!(usd_field(&cg), Some(0.02189269));
        // A flatter shape an override might return.
        assert_eq!(usd_field(&serde_json::json!({"USD": 1.5})), Some(1.5));
        assert_eq!(usd_field(&serde_json::json!({"price": {"eur": 1.0}})), None);
        assert_eq!(usd_field(&serde_json::json!({"usd": "n/a"})), None);
    }

    #[test]
    fn price_url_defaults_and_can_be_turned_off() {
        // (Env is process-global; restored below.)
        std::env::remove_var("NOCKMARK_PRICE_URL");
        assert_eq!(price_url().as_deref(), Some(DEFAULT_PRICE_URL));
        std::env::set_var("NOCKMARK_PRICE_URL", "off");
        assert_eq!(price_url(), None);
        std::env::set_var("NOCKMARK_PRICE_URL", "  ");
        assert_eq!(price_url(), None);
        std::env::set_var("NOCKMARK_PRICE_URL", "https://example.test/p");
        assert_eq!(price_url().as_deref(), Some("https://example.test/p"));
        std::env::remove_var("NOCKMARK_PRICE_URL");
    }

    #[test]
    fn signif4_rounds_sanely() {
        assert_eq!(signif4(123_456.0), 123_500.0);
        assert_eq!(signif4(0.000123456), 0.0001235);
        assert_eq!(signif4(0.0), 0.0);
        assert_eq!(signif4(1.0), 1.0);
    }

    /// nockscan `/api/v1/mining-metrics` as it answered on 2026-11-11 at
    /// height 181,403 (unused fields trimmed).
    fn nockscan_fixture() -> serde_json::Value {
        serde_json::json!({
            "latestBlock": 181403,
            "latestBlockTimestamp": 1794437006u64,
            "difficulty": "2^46.470",
            "log2Difficulty": 46.46974668288385,
            "aiDifficulty": "2^71.471",
            "aiLog2Difficulty": 71.4713625784859,
            "tipPuzzle": "zk",
            "proofsPerSecondTarget": 194902602457.37595,
            "blockRewardNock": 1536,
            "workNormalization": { "macsPerZkUnit": 14285714, "unit": "Tip5 hash", "sinceHeight": 154500 }
        })
    }

    #[test]
    fn nockscan_mining_metrics_is_read_in_full() {
        let tips = tips_from_nockscan(&nockscan_fixture(), "test").unwrap();
        assert_eq!(tips.len(), 3, "ZK, AI and reward");
        match &tips[0] {
            Tip::Zk { difficulty, tip_timestamp } => {
                // 2^46.4697 ≈ 9.746e13 Tip5 hashes per ZK block; over the
                // 500 s ideal that is nockscan's own proofsPerSecondTarget.
                assert!((difficulty / 500.0 - 194_902_602_457.375_95).abs() / 1.95e11 < 1e-9);
                assert_eq!(*tip_timestamp, Some(1_794_437_006), "the tip is a ZK block");
            }
            _ => panic!("first tip must be ZK"),
        }
        match &tips[1] {
            Tip::Ai { mac_equiv_per_block, target, tip_timestamp } => {
                assert!((mac_equiv_per_block.log2() - 71.471_362_578_485_9).abs() < 1e-9);
                assert_eq!(*target, None, "nockscan gives log2, not the target");
                assert_eq!(*tip_timestamp, None, "the tip block is ZK, so AI gets no block time");
            }
            _ => panic!("second tip must be AI"),
        }
        assert!(matches!(tips[2], Tip::Reward { total_nock } if total_nock == 1_536.0));
    }

    #[test]
    fn nockscan_out_of_band_figures_are_refused() {
        let mut v = nockscan_fixture();
        v["log2Difficulty"] = serde_json::json!(100.0);
        assert!(tips_from_nockscan(&v, "test").is_err(), "no legal ZK target gives 2^100");
        let mut v = nockscan_fixture();
        v["aiLog2Difficulty"] = serde_json::json!(10.0);
        assert!(tips_from_nockscan(&v, "test").is_err(), "no legal AI target gives 2^10");
        let mut v = nockscan_fixture();
        v["log2Difficulty"] = serde_json::json!("2^46.470");
        assert!(tips_from_nockscan(&v, "test").is_err(), "the display string is not the number");
        // An implausible reward is dropped, not fatal: difficulty still reads.
        let mut v = nockscan_fixture();
        v["blockRewardNock"] = serde_json::json!(0);
        let tips = tips_from_nockscan(&v, "test").unwrap();
        assert_eq!(tips.len(), 2);
        assert!(!tips.iter().any(|t| matches!(t, Tip::Reward { .. })));
    }

    #[tokio::test]
    async fn reward_is_the_miner_share_and_says_when_it_was_seen() {
        let econ = Arc::new(RwLock::new(Some(params(1_000.0, 1_638.4))));
        assert_eq!(econ.read().await.as_ref().unwrap().block_reward_observed_at, None);
        apply_tip(Tip::Reward { total_nock: 1_536.0 }, "test", &econ, None).await;
        let p = econ.read().await.clone().unwrap();
        // 0.8 × 1536 — what block 181,396's coinbase actually paid its miner —
        // and exactly the double 1228.8, so JSON prints "1228.8", not
        // "1228.8000000000002".
        assert_eq!(p.block_reward_nock, 1_228.8);
        assert_eq!(serde_json::json!(p.block_reward_nock).to_string(), "1228.8");
        assert!(p.block_reward_observed_at.is_some());
    }

    #[test]
    fn source_precedence() {
        let s = |x: &str| Some(x.to_string());
        let get = |u: &str| Source::Get(u.into());
        let rpc = |u: &str| Source::Rpc { url: u.into(), api_key: "k".into() };
        // Both configured: nockscan first, NockBlocks as fallback.
        assert_eq!(sources(s("ns"), s("nb"), s("k")), vec![get("ns"), rpc("nb")]);
        // The pre-M7 production config still means RPC.
        assert_eq!(sources(s("nb"), None, s("k")), vec![rpc("nb")]);
        assert_eq!(sources(s("ns"), None, None), vec![get("ns")]);
        assert_eq!(sources(None, s("nb"), s("k")), vec![rpc("nb")]);
        // An RPC URL without a key is unusable, so it is not a source.
        assert_eq!(sources(None, s("nb"), None), vec![]);
        assert_eq!(sources(None, None, None), vec![]);
    }
}
