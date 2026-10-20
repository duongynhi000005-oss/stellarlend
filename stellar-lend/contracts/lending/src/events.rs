//! Contract events for the lending workspace (`LendingContract`, `DataStore`, `UpgradeManager`).
//!
//! Events are emitted via the Soroban `contractevent` macro, which routes to the typed event
//! publishing helper. Event names are derived from the struct identifier (max 32 bytes).
//!
//! `BorrowEvent`, `RepayEvent`, and the other protocol-wide events are re-exported from
//! [`shared_events`]; lending-specific events live below.
//!
//! # Indexer / off-chain consumers
//!
//! - **Lending (main contract)** — `borrow_event`, `repay_event`, `withdraw_event`,
//!   `flash_loan_event`, `deposit_event` (vault vs borrow-collateral variants), plus
//!   peg-deviation, stability-fee, utilization-alert and utilization-breaker events.
//! - **Data store contract** — `ds_init`, `ds_save`, `ds_bkup`, `ds_rest`, `ds_migr`, `ds_writer`.
//!
//! # Deprecation notes
//!
//! The legacy `emit_bad_debt` / `emit_bad_debt_recovered` helpers below publish raw events via
//! `Env::events().publish` (deprecated). They are kept for backward compatibility with existing
//! off-chain consumers and are gated with `#[allow(deprecated)]`.

pub use shared_events::*;

use soroban_sdk::{contractevent, contracttype, Address, BytesN, Env, String};

// ─── Lending (LendingContract) ─────────────────────────────────────────────

/// Collateral added to a borrow position (static topic `deposit_event`, payload without
/// `new_balance`).
#[contractevent]
#[derive(Clone, Debug)]
pub struct BorrowCollateralDepositEvent {
    #[topic]
    pub user: Address,
    pub asset: Address,
    pub amount: i128,
    pub timestamp: u64,
}

/// Liquidation incentive changed. `asset` is the collateral asset whose
/// override changed, or `None` for the global incentive. `new_bps` is the
/// incentive now in effect, which falls back to the global value when an
/// override is cleared.
#[contractevent]
#[derive(Clone, Debug)]
pub struct LiqIncentiveUpdatedEvent {
    #[topic]
    pub admin: Address,
    pub asset: Option<Address>,
    pub previous_bps: i128,
    pub new_bps: i128,
    pub timestamp: u64,
}

/// Per-asset borrow cap updated by admin (`new_cap` is `0` when the per-asset cap is cleared).
#[contractevent]
#[derive(Clone, Debug)]
pub struct BorrowCapUpdatedEvent {
    #[topic]
    pub admin: Address,
    pub asset: Address,
    pub previous_cap: i128,
    pub new_cap: i128,
    pub timestamp: u64,
}

/// Interest rate model parameters changed (admin or governance action).
#[contractevent]
#[derive(Clone, Debug)]
pub struct RateConfigUpdatedEvent {
    #[topic]
    pub caller: Address,
    pub base_rate_bps: i128,
    pub kink_utilization_bps: i128,
    pub slope_bps: i128,
    pub jump_slope_bps: i128,
    pub rate_floor_bps: i128,
    pub rate_ceiling_bps: i128,
    pub spread_bps: i128,
    pub timestamp: u64,
}

/// Vault / pool deposit (static topic `deposit_event`, payload includes `new_balance`).
#[contractevent]
#[derive(Clone, Debug)]
pub struct VaultDepositEvent {
    #[topic]
    pub user: Address,
    pub asset: Address,
    pub amount: i128,
    pub new_balance: i128,
    pub timestamp: u64,
}

/// Summary of a `deposit_batch` call. Each entry in the batch still emits its
/// own [`VaultDepositEvent`]; this event lets indexers group them.
#[contractevent]
#[derive(Clone, Debug)]
pub struct BatchDepositEvent {
    #[topic]
    pub user: Address,
    /// Number of deposits applied.
    pub count: u32,
    /// Sum of all deposited amounts.
    pub total_amount: i128,
    /// User's collateral balance after the whole batch.
    pub new_balance: i128,
    pub timestamp: u64,
}

/// Withdraw / emergency-withdraw event (includes post-withdrawal balance).
#[contractevent]
#[derive(Clone, Debug)]
pub struct WithdrawEvent {
    #[topic]
    pub user: Address,
    pub asset: Address,
    pub amount: i128,
    pub remaining_balance: i128,
    pub timestamp: u64,
}

/// Flash loan repayment completed.
#[contractevent]
#[derive(Clone, Debug)]
pub struct FlashLoanEvent {
    #[topic]
    pub receiver: Address,
    #[topic]
    pub asset: Address,
    pub amount: i128,
    pub fee: i128,
    pub timestamp: u64,
}

/// Oracle peg deviation detected during interest accrual.
#[contractevent]
#[derive(Clone, Debug)]
pub struct PegDeviationEvent {
    #[topic]
    pub asset: Address,
    pub price: i128,
    pub target_price: i128,
    pub deviation_bps: i128,
    pub timestamp: u64,
}

/// Stability fee applied to a borrow position.
#[contractevent]
#[derive(Clone, Debug)]
pub struct StabilityFeeAppliedEvent {
    #[topic]
    pub asset: Address,
    pub fee_bps: i128,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RiskAlertSeverity {
    Warning = 1,
    Critical = 2,
    Emergency = 3,
}

/// Utilization crossed a configured risk tier (deduplicated by escalation).
#[contractevent]
#[derive(Clone, Debug)]
pub struct RiskUtilizationAlertEvent {
    pub severity: u32,
    pub utilization_bps: u32,
    pub total_debt: i128,
    pub debt_ceiling: i128,
    pub timestamp: u64,
}

// ─── Code upgrades ──────────────────────────────────────────────────────────

/// Lending contract code was replaced via `upgrade` (also used for rollbacks).
#[contractevent]
#[derive(Clone, Debug)]
pub struct ContractUpgradedEvent {
    #[topic]
    pub admin: Address,
    pub new_wasm_hash: BytesN<32>,
    pub timestamp: u64,
}

// ─── Utilization circuit breaker ────────────────────────────────────────────

/// Breaker configuration installed or changed by the admin.
#[contractevent]
#[derive(Clone, Debug)]
pub struct UtilBreakerConfiguredEvent {
    #[topic]
    pub admin: Address,
    pub enabled: bool,
    pub trip_utilization_bps: u32,
    pub reset_utilization_bps: u32,
    pub max_borrow_while_tripped: i128,
    pub timestamp: u64,
}

/// Utilization reached the trip threshold; new borrows are now throttled.
#[contractevent]
#[derive(Clone, Debug)]
pub struct UtilBreakerTrippedEvent {
    pub utilization_bps: u32,
    pub trip_utilization_bps: u32,
    pub total_debt: i128,
    pub debt_ceiling: i128,
    pub timestamp: u64,
}

/// Breaker re-armed, either automatically (utilization fell below the reset threshold) or by
/// the admin (`manual = true`).
#[contractevent]
#[derive(Clone, Debug)]
pub struct UtilBreakerResetEvent {
    pub utilization_bps: u32,
    pub manual: bool,
    pub timestamp: u64,
}

// ─── Commitments ────────────────────────────────────────────────────────────

#[contractevent]
#[derive(Clone, Debug)]
pub struct BorrowCommitmentCreatedEvent {
    #[topic]
    pub commitment_id: u64,
    #[topic]
    pub owner: Address,
    pub borrow_asset: Address,
    pub borrow_amount: i128,
    pub expiry: u64,
}

#[contractevent]
#[derive(Clone, Debug)]
pub struct CommitmentCancelledEvent {
    #[topic]
    pub commitment_id: u64,
    #[topic]
    pub owner: Address,
    pub timestamp: u64,
}

#[contractevent]
#[derive(Clone, Debug)]
pub struct BorrowCommitmentExecutedEvent {
    #[topic]
    pub commitment_id: u64,
    #[topic]
    pub owner: Address,
    pub borrowed_amount: i128,
    pub collateral_amount: i128,
}

// ─── Data store contract ────────────────────────────────────────────────────

#[contractevent]
#[derive(Clone, Debug)]
pub struct DataStoreInitEvent {
    #[topic]
    pub admin: Address,
}

#[contractevent]
#[derive(Clone, Debug)]
pub struct DataStoreWriterChangeEvent {
    #[topic]
    pub caller: Address,
    #[topic]
    pub writer: Address,
}

#[contractevent]
#[derive(Clone, Debug)]
pub struct DataStoreSaveEvent {
    #[topic]
    pub caller: Address,
    pub key: String,
    pub value_len: u32,
}

#[contractevent]
#[derive(Clone, Debug)]
pub struct DataStoreBackupEvent {
    #[topic]
    pub caller: Address,
    #[topic]
    pub backup_name: String,
    pub key_count: u32,
}

#[contractevent]
#[derive(Clone, Debug)]
pub struct DataStoreRestoreEvent {
    #[topic]
    pub caller: Address,
    #[topic]
    pub backup_name: String,
    pub entry_count: u32,
}

#[contractevent]
#[derive(Clone, Debug)]
pub struct DataStoreMigrateEvent {
    #[topic]
    pub caller: Address,
    pub new_version: u32,
    pub memo: Option<String>,
}

// ─── Legacy helpers (kept for backward compatibility) ──────────────────────

#[allow(deprecated)]
pub fn emit_bad_debt(env: &Env, user: &Address, amount: i128) {
    env.events().publish(("bad_debt",), (user.clone(), amount));
}

#[allow(deprecated)]
pub fn emit_bad_debt_recovered(env: &Env, amount: i128) {
    env.events().publish(("bad_debt_recovered",), (amount,));
}
