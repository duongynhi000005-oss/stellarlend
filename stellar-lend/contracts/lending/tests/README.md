# Lending contract integration suites

These suites build against the contract's public client (`LendingContractClient`), the same interface that wallets, the API, and the benchmarks call. Because they compile independently of the crate's `#[cfg(test)]` modules, they run even when those modules don't build.

| Suite | Issue | What it checks |
|---|---|---|
| `contract_invariants.rs` | #1361 | Property-based contract invariants for supply caps, liquidations, batch deposits, pauses, and multi-collateral positions |
| `fuzz_invariants.rs` | #686 | Property-based fuzzing of deposit, withdraw, borrow, and repay, with invariants checked after every call |
| `user_journeys.rs` | #693 | Full user journeys with Soroban gas metered per step and checked against `benchmarks/baseline.json` budgets |
| `event_topics.rs` | #685 | The event layout the off-chain indexer depends on |
| `protocol_formal_specs.rs` | — | Formal specification lemmas |
| `scenarios.rs` | #700 | JSON scenarios from `test-utils/scenarios/`, plus seeded multi-user and price-path checks against the reference health-factor model |
| `config_auth.rs`, `admin_config_auth.rs` | — | Admin-only configuration entry points reject callers without auth |
| `incident_regressions.rs` | #1440 | One regression test per past incident, indexed in [`docs/INCIDENT_REGRESSIONS.md`](../../../../docs/INCIDENT_REGRESSIONS.md) |

Shared setup lives in `common/mod.rs` and builds on the `test-utils` framework: `snapshotless_env()`, `PriceOracle`, and `GasMeter`/`GasBudgets` for journey metering. See [`../../../TESTING.md`](../../../TESTING.md) for the unified runner (`scripts/testing/run-contract-tests.sh`), coverage and CI.

Run everything, with timing and a performance report:

```bash
scripts/testing/run-property-suite.sh                  # PROPTEST_CASES=64 by default
PROPTEST_CASES=1024 scripts/testing/run-property-suite.sh
```

Run the suites directly:

```bash
cd stellar-lend
cargo test -p stellarlend-lending --test fuzz_invariants --test user_journeys --test event_topics
```

## Contract invariants (#1361, `contract_invariants.rs`)

Formally defines, fuzzes, and enforces protocol invariants for the lending contract:

| ID | Name | Invariant |
|---|---|---|
| INV-01 | Supply Cap Invariant | For all assets $a$, $\text{total\_deposited}(a) \le \text{supply\_cap}(a)$ when cap is set; deposits exceeding the cap are rejected |
| INV-02 | Liquidation Solvency & Conservation | Liquidation requires $HF < 1.0$; debt strictly decreases, collateral seized matches debt repaid + incentive, self-liquidation rejected |
| INV-03 | Batch Deposit Atomicity | Batch deposits apply all entries atomically or revert completely with 0 state change on cap violation |
| INV-04 | Subsystem Pause Safety | Paused operations (`Deposit`, `Borrow`, `Repay`, `Withdraw`, `Liquidation`) fail immediately without touching state |
| INV-05 | Multi-Collateral Valuation | Reported collateral value equals $\sum_i \text{amount}_i \cdot \text{price}_i / 10^8$, capped at 8 distinct collateral assets |
| INV-06 | Accounting Soundness | Non-negative balances for all users; debt-free users return health factor sentinel $100{,}000{,}000$ |

## Fuzzing and invariants (`fuzz_invariants.rs`)

Each proptest case runs a random sequence of up to 60 calls across 3 users. Amounts are chosen to hit both sides of the protocol minimums and the 150% collateral boundary, and include zero and negative values. After every call, the suite checks:

| ID | Invariant |
|---|---|
| I1 | No balance is ever negative |
| I2 | Deposit balances match an independent model |
| I3 | Total outstanding principal stays at or below the debt ceiling |
| I4 | Every position stays at least 150% collateralized, using the contract's floor rounding |
| I5 | Health factor stays at or above 1.0 while prices are flat and no time passes |
| I6 | A rejected call leaves all observable state unchanged |
| I7 | Debt never falls over time or from deposits, withdrawals, or borrows |

On a violation, the suite writes `target/invariant-violations/<invariant>.json` (override the location with `INVARIANT_REPORT_DIR`) and panics with `INVARIANT VIOLATION`. Proptest then shrinks the sequence to a minimal reproducer. On `main` and on the nightly deep run, `.github/workflows/property-tests.yml` opens an issue labelled `invariant-violation`, or comments on the existing one. On pull requests it comments on the PR.

### Findings

The fuzzer found two issues. Both are left for maintainers to decide on, because fixing them changes protocol behavior:

1. **Collateral check rounds in the borrower's favor.** `validate_collateral_ratio` uses `floor(borrow × 1.5)`, so a borrow can be up to one base unit short of exactly 150% (for example, borrowing 409,429 against 614,143 collateral). Formal spec lemma C-06 describes this floor rounding as intended. `collateral_floor_rounding_is_sub_unit` pins the behavior and bounds the shortfall below one unit.
2. **Variable interest is repriced retroactively.** `calculate_interest` applies the *current* utilization rate to the whole period since each borrower's last update. When one user repays, every other borrower's accrued interest drops, and when one user borrows, it rises. The ignored test `known_issue_variable_interest_repriced_retroactively` asserts the correct behavior. It will pass once a global borrow index is added. Run it with `cargo test --test fuzz_invariants -- --ignored`.

## Journeys and gas (`user_journeys.rs`)

Each journey runs deposit → borrow → check position → repay → withdraw, for a single user and for five interleaved users. It also covers recovery from paused withdrawals and deposits, an under-collateralized borrow, and an over-repay. Every step runs under a fresh Soroban budget.

- **Budgets:** state-changing steps must fit their `gas_budgets` entry. Read-only views are measured and reported, but not enforced, because clients call them through RPC simulation.
- **Report:** `target/journey-reports/*.json` holds the per-step CPU instructions and memory, wall time, and recommendations. `api/src/services/gasReport` picks this report up (#684).
- **Currently over budget:** `get_user_position` measures about 410k instructions against its 400k budget. The report flags it; the budget has not been changed here.
