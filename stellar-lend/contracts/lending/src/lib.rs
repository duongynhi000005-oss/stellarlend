#![no_std]

// `alloc` is not in the extern prelude for a `no_std` crate. Declaring it at the
// crate root makes `alloc::` paths usable from every module in the tree,
// including the test-only property/invariant harnesses and the spec modules.
// Gated to those builds: the contract itself never allocates through `alloc`, and an
// unconditional declaration breaks the wasm build (no global allocator on wasm32v1-none).
#[cfg(any(test, feature = "spec"))]
extern crate alloc;

use soroban_sdk::{contract, contractimpl, Address, Bytes, BytesN, Env, Map, Val, Vec};

mod borrow;
mod calldata;
mod deposit;
mod deposit_batch;
mod dust;
mod events;
mod flash_loan;
mod hot_storage;
mod interest_rate;
mod lazy;
mod liquidate;
mod pause;
mod reentrancy;
mod risk_monitor;
mod supplier_rewards;
pub use supplier_rewards::{
    SupplierReward, SupplierRewardPool, SupplierRewardsConfig, SupplierRewardsError,
};
mod token_receiver;
mod utilization_breaker;
mod withdraw;

// Property-based fuzzing and invariant-testing support. These modules existed in
// the tree but were never declared here, so `invariant_prop_test.rs` could not
// resolve `crate::proptest_helpers` / `crate::invariant_test_suite` and the whole
// lending test target failed to compile. They are test-only: nothing in the
// non-test build depends on them.
#[cfg(test)]
mod invariant_test_suite;
#[cfg(test)]
mod invariants;
#[cfg(test)]
mod proptest_helpers;
#[cfg(test)]
mod state_machine;

use borrow::{
    borrow as borrow_cmd, borrow_with_rate as borrow_with_rate_logic, deposit as borrow_deposit,
    get_admin as get_borrow_admin, get_user_collateral as get_borrow_collateral,
    get_user_debt as get_borrow_debt, get_user_debt_with_rate as get_borrow_debt_with_rate,
    initialize_borrow_settings as initialize_borrow_logic, repay as borrow_repay,
    set_admin as set_borrow_admin,
    set_liquidation_threshold_bps as set_liquidation_threshold_logic,
    set_oracle as set_oracle_logic, set_variable_borrow_rate_bps as set_variable_borrow_rate_logic,
    sweep_debt_dust as borrow_sweep_debt_dust, switch_rate_type as switch_rate_type_logic,
    BorrowCollateral, DebtPosition, RateType,
};
pub use borrow::{
    BorrowError, DEFAULT_LIQUIDATION_INCENTIVE_BPS, MAX_COLLATERAL_ASSETS,
    MAX_LIQUIDATION_INCENTIVE_BPS, MIN_LIQUIDATION_INCENTIVE_BPS,
};
use calldata::{CalldataError, CompressedOp};
use deposit::{
    deposit as deposit_logic, get_asset_supply_cap as get_asset_supply_cap_logic,
    get_asset_total_supply as get_asset_total_supply_logic,
    get_user_collateral as get_deposit_collateral,
    initialize_deposit_settings as initialize_deposit_logic,
    set_asset_supply_cap as set_asset_supply_cap_logic,
};
pub use deposit::{DepositCollateral, DepositError};
use deposit_batch::deposit_batch as deposit_batch_logic;
pub use deposit_batch::{BatchDepositResult, DepositRequest, MAX_BATCH_DEPOSITS};
use flash_loan::{
    flash_loan as flash_loan_logic, get_flash_loan_fee_bps as get_flash_loan_fee_bps_logic,
    get_flash_loan_security_config as get_flash_loan_security_config_logic,
    get_flash_loan_security_snapshot as get_flash_loan_security_snapshot_logic,
    is_asset_flash_loan_active as is_asset_flash_loan_active_logic,
    set_flash_loan_fee_bps as set_flash_loan_fee_logic,
    set_flash_loan_security_config as set_flash_loan_security_config_logic, FlashLoanError,
    FlashLoanSecurityConfig, FlashLoanSecuritySnapshot,
};
pub use interest_rate::{InterestRateConfig, InterestRateConfigUpdate};
use lazy::{LazyField, PoolStateView};
pub use liquidate::LiquidationResult;
/// Re-exported so integrators (and the `tests/` suites) can build `set_pause`
/// and `guardian_pause` arguments.
pub use pause::PauseType;
use pause::{is_paused, set_pause as set_pause_logic};
use reentrancy::ReentrancyGuard;
use token_receiver::receive as receive_logic;
use utilization_breaker::{
    UtilizationBreakerConfig, UtilizationBreakerError, UtilizationBreakerStatus,
};

mod views;
use views::{
    get_collateral_balance as view_collateral_balance,
    get_collateral_value as view_collateral_value, get_debt_balance as view_debt_balance,
    get_debt_value as view_debt_value, get_health_factor as view_health_factor,
    get_user_position as view_user_position, UserPositionSummary,
};

pub use withdraw::WithdrawError;
use withdraw::{
    emergency_withdraw as emergency_withdraw_logic,
    get_emergency_stats as get_emergency_stats_logic,
    initialize_withdraw_settings as initialize_withdraw_logic,
    set_emergency_withdraw_limit as set_emergency_withdraw_limit_logic,
    set_withdraw_paused as set_withdraw_paused_logic,
    sweep_deposit_dust as sweep_deposit_dust_logic, withdraw as withdraw_logic,
};
mod data_store;
mod insurance;
mod upgrade;

use insurance::{
    cancel_claim as insurance_cancel_claim, collect_premium as insurance_collect_premium,
    evaluate_claim as insurance_evaluate_claim, fund_pool as insurance_fund_pool,
    get_all_claim_ids as insurance_get_all_claim_ids, get_all_claims as insurance_get_all_claims,
    get_analytics as insurance_get_analytics, get_claim_by_id as insurance_get_claim,
    get_coverage_limit as insurance_get_coverage_limit,
    get_premium_rate as insurance_get_premium_rate, initialize as insurance_initialize,
    set_coverage_limit as insurance_set_coverage_limit, submit_claim as insurance_submit_claim,
    InsuranceAnalytics, InsuranceClaim, InsuranceError,
};

#[cfg(test)]
mod borrow_test;
#[cfg(test)]
mod contract_upgrade_test;
#[cfg(test)]
mod data_store_test;
#[cfg(test)]
mod deposit_test;
#[cfg(test)]
mod dust_test;
#[cfg(test)]
mod flash_loan_test;
#[cfg(test)]
mod initialize_test;
#[cfg(test)]
mod insurance_test;
#[cfg(test)]
mod invariant_prop_test;
#[cfg(test)]
mod liquidation_boundary_test;
#[cfg(test)]
mod math_safety_test;
#[cfg(test)]
mod pause_test;
#[cfg(any(test, feature = "spec"))]
mod spec;
#[cfg(test)]
mod token_receiver_test;
#[cfg(test)]
mod upgrade_test;
#[cfg(test)]
mod utilization_breaker_test;
#[cfg(test)]
mod views_test;
#[cfg(test)]
mod withdraw_test;

#[contract]
pub struct LendingContract;

#[contractimpl]
impl LendingContract {
    /// Admin selects an immutable reward token and enables/disables new allocations.
    pub fn configure_supplier_rewards(
        env: Env,
        admin: Address,
        token: Address,
        enabled: bool,
    ) -> Result<(), SupplierRewardsError> {
        supplier_rewards::configure(&env, admin, token, enabled)
    }

    /// Deposit actual reward tokens; only these funds can back new entitlements.
    pub fn fund_supplier_rewards(
        env: Env,
        funder: Address,
        amount: i128,
    ) -> Result<(), SupplierRewardsError> {
        supplier_rewards::fund(&env, funder, amount)
    }

    /// Admin sets a supplier's cumulative entitlement, after verifying eligibility.
    /// Repeating the same total never allocates or pays twice.
    pub fn set_supplier_reward(
        env: Env,
        admin: Address,
        user: Address,
        total: i128,
    ) -> Result<(), SupplierRewardsError> {
        supplier_rewards::allocate(&env, admin, user, total)
    }

    /// Claim vested tokens to the authorizing supplier, including after withdrawal
    /// or disabling new allocations. A repeated claim returns zero.
    pub fn claim_supplier_rewards(env: Env, user: Address) -> Result<i128, SupplierRewardsError> {
        supplier_rewards::claim(&env, user)
    }

    pub fn get_supplier_reward(env: Env, user: Address) -> SupplierReward {
        supplier_rewards::reward(&env, &user)
    }

    pub fn get_supplier_reward_pool(env: Env) -> SupplierRewardPool {
        supplier_rewards::pool(&env)
    }

    pub fn get_supplier_rewards_config(env: Env) -> Option<SupplierRewardsConfig> {
        supplier_rewards::config(&env)
    }

    /// Initialize the protocol with admin and settings
    pub fn initialize(
        env: Env,
        admin: Address,
        debt_ceiling: i128,
        min_borrow_amount: i128,
    ) -> Result<(), BorrowError> {
        let _guard =
            ReentrancyGuard::new_constructor(&env).map_err(|_| BorrowError::ReentrancyDetected)?;

        if get_borrow_admin(&env).is_some() {
            return Err(BorrowError::Unauthorized);
        }
        admin.require_auth();
        set_borrow_admin(&env, &admin);
        initialize_borrow_logic(&env, debt_ceiling, min_borrow_amount)?;
        Ok(())
    }

    /// Borrow assets against deposited collateral
    pub fn borrow(
        env: Env,
        user: Address,
        asset: Address,
        amount: i128,
        collateral_asset: Address,
        collateral_amount: i128,
    ) -> Result<(), BorrowError> {
        borrow_cmd(
            &env,
            user,
            asset,
            amount,
            collateral_asset,
            collateral_amount,
        )
    }

    /// Borrow assets with explicit variable or stable rate selection.
    pub fn borrow_with_rate(
        env: Env,
        user: Address,
        asset: Address,
        amount: i128,
        collateral_asset: Address,
        collateral_amount: i128,
        rate_type: RateType,
    ) -> Result<(), BorrowError> {
        borrow_with_rate_logic(
            &env,
            user,
            asset,
            amount,
            collateral_asset,
            collateral_amount,
            rate_type,
        )
    }

    /// Switch an existing debt position between variable and stable rates.
    pub fn switch_rate_type(
        env: Env,
        user: Address,
        asset: Address,
        to_rate_type: RateType,
    ) -> Result<(), BorrowError> {
        switch_rate_type_logic(&env, user, asset, to_rate_type)
    }

    /// Set the variable borrow rate model base rate.
    pub fn set_variable_borrow_rate_bps(
        env: Env,
        admin: Address,
        rate_bps: i128,
    ) -> Result<(), BorrowError> {
        set_variable_borrow_rate_logic(&env, &admin, rate_bps)
    }

    /// Set protocol pause state for a specific operation (admin only)
    pub fn set_pause(
        env: Env,
        admin: Address,
        pause_type: PauseType,
        paused: bool,
    ) -> Result<(), BorrowError> {
        let current_admin = get_borrow_admin(&env).ok_or(BorrowError::Unauthorized)?;
        if admin != current_admin {
            return Err(BorrowError::Unauthorized);
        }
        admin.require_auth();
        set_pause_logic(&env, admin, pause_type, paused);
        Ok(())
    }

    /// Replace this contract's code with an already-uploaded wasm (admin only).
    ///
    /// Storage is preserved; the new code must stay storage-compatible (see
    /// `docs/runbooks/upgrade-and-migration.md`). Rolling back is the same call with the
    /// previous wasm hash.
    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), BorrowError> {
        let current_admin = get_borrow_admin(&env).ok_or(BorrowError::Unauthorized)?;
        if admin != current_admin {
            return Err(BorrowError::Unauthorized);
        }
        admin.require_auth();
        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        events::ContractUpgradedEvent {
            admin,
            new_wasm_hash,
            timestamp: env.ledger().timestamp(),
        }
        .publish(&env);
        Ok(())
    }

    /// Immediately pause or resume every state-changing protocol operation.
    ///
    /// This is the emergency control surface. It is restricted to the
    /// configured admin and delegates to the same guarded pause storage used
    /// by the granular operation controls.
    pub fn emergency_pause(env: Env, admin: Address, paused: bool) -> Result<(), BorrowError> {
        Self::set_pause(env, admin, PauseType::All, paused)
    }

    /// Repay borrowed assets
    pub fn repay(env: Env, user: Address, asset: Address, amount: i128) -> Result<(), BorrowError> {
        user.require_auth();
        if is_paused(&env, PauseType::Repay) {
            return Err(BorrowError::ProtocolPaused);
        }
        borrow_repay(&env, user, asset, amount)
    }

    /// Sweep a dust-sized debt balance that is below the minimum borrow size.
    pub fn sweep_debt_dust(env: Env, user: Address, asset: Address) -> Result<i128, BorrowError> {
        user.require_auth();
        if is_paused(&env, PauseType::Repay) {
            return Err(BorrowError::ProtocolPaused);
        }
        borrow_sweep_debt_dust(&env, user, asset)
    }

    /// Deposit collateral into the protocol
    pub fn deposit(
        env: Env,
        user: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, DepositError> {
        if is_paused(&env, PauseType::Deposit) {
            return Err(DepositError::DepositPaused);
        }
        deposit_logic(&env, user, asset, amount)
    }

    /// Apply up to `MAX_BATCH_DEPOSITS` deposits atomically with a single auth
    /// check and a single write of shared pool state.
    pub fn deposit_batch(
        env: Env,
        user: Address,
        requests: Vec<DepositRequest>,
    ) -> Result<BatchDepositResult, DepositError> {
        if is_paused(&env, PauseType::Deposit) {
            return Err(DepositError::DepositPaused);
        }
        deposit_batch_logic(&env, user, requests)
    }

    /// Deposit collateral for a borrow position
    pub fn deposit_collateral(
        env: Env,
        user: Address,
        asset: Address,
        amount: i128,
    ) -> Result<(), BorrowError> {
        user.require_auth();
        if is_paused(&env, PauseType::Deposit) {
            return Err(BorrowError::ProtocolPaused);
        }
        borrow_deposit(&env, user, asset, amount)
    }

    /// Liquidate an unhealthy position: repay up to `amount` of the
    /// borrower's `debt_asset` debt (capped by the close factor) and seize
    /// `collateral_asset` worth the repaid value plus that asset's
    /// liquidation incentive. The health factor covers every collateral
    /// asset the borrower holds.
    pub fn liquidate(
        env: Env,
        liquidator: Address,
        borrower: Address,
        debt_asset: Address,
        collateral_asset: Address,
        amount: i128,
    ) -> Result<LiquidationResult, BorrowError> {
        liquidator.require_auth();
        if is_paused(&env, PauseType::Liquidation) {
            return Err(BorrowError::ProtocolPaused);
        }
        liquidate::liquidate(
            &env,
            liquidator,
            borrower,
            debt_asset,
            collateral_asset,
            amount,
        )
    }

    /// Get user's debt position
    pub fn get_user_debt(env: Env, user: Address) -> DebtPosition {
        get_borrow_debt(&env, &user)
    }

    /// Get a user's debt position for a specific rate bucket.
    pub fn get_user_debt_with_rate(env: Env, user: Address, rate_type: RateType) -> DebtPosition {
        get_borrow_debt_with_rate(&env, &user, rate_type)
    }

    /// Get user's collateral position (borrow module). For a position with
    /// several collateral assets this is the asset with the largest balance;
    /// use `get_user_collaterals` for all of them.
    pub fn get_user_collateral(env: Env, user: Address) -> BorrowCollateral {
        get_borrow_collateral(&env, &user)
    }

    /// Every collateral asset backing the user's borrow position, with its
    /// raw balance.
    pub fn get_user_collaterals(env: Env, user: Address) -> Map<Address, i128> {
        borrow::get_collateral_balances(&env, &user)
    }

    // ═══════════════════════════════════════════════════════════════════
    // View functions (read-only; for frontends and liquidations)
    // ═══════════════════════════════════════════════════════════════════

    /// Returns the user's collateral balance (raw amount).
    pub fn get_collateral_balance(env: Env, user: Address) -> i128 {
        view_collateral_balance(&env, &user)
    }

    /// Returns the user's debt balance (principal + accrued interest).
    pub fn get_debt_balance(env: Env, user: Address) -> i128 {
        view_debt_balance(&env, &user)
    }

    /// Returns the user's collateral value in common unit (e.g. USD 8 decimals). 0 if oracle not set.
    pub fn get_collateral_value(env: Env, user: Address) -> i128 {
        view_collateral_value(&env, &user)
    }

    /// Returns the user's debt value in common unit. 0 if oracle not set.
    pub fn get_debt_value(env: Env, user: Address) -> i128 {
        view_debt_value(&env, &user)
    }

    /// Returns health factor (scaled 10000 = 1.0). Above 10000 = healthy; below = liquidatable.
    pub fn get_health_factor(env: Env, user: Address) -> i128 {
        view_health_factor(&env, &user)
    }

    /// Returns full position summary: collateral/debt balances and values, and health factor.
    pub fn get_user_position(env: Env, user: Address) -> UserPositionSummary {
        view_user_position(&env, &user)
    }

    /// Set oracle address for price feeds (admin only).
    pub fn set_oracle(env: Env, admin: Address, oracle: Address) -> Result<(), BorrowError> {
        set_oracle_logic(&env, &admin, oracle)
    }

    /// Set liquidation threshold in basis points, e.g. 8000 = 80% (admin only).
    pub fn set_liquidation_threshold_bps(
        env: Env,
        admin: Address,
        bps: i128,
    ) -> Result<(), BorrowError> {
        set_liquidation_threshold_logic(&env, &admin, bps)
    }

    /// Set the global liquidation incentive in basis points (admin only),
    /// applied to collateral assets without an override. Must be within
    /// `MIN_LIQUIDATION_INCENTIVE_BPS..=MAX_LIQUIDATION_INCENTIVE_BPS`.
    pub fn set_liquidation_incentive_bps(
        env: Env,
        admin: Address,
        bps: i128,
    ) -> Result<(), BorrowError> {
        borrow::set_liquidation_incentive_bps(&env, &admin, bps)
    }

    /// Global liquidation incentive in basis points.
    pub fn get_liquidation_incentive_bps(env: Env) -> i128 {
        borrow::get_liquidation_incentive_bps(&env)
    }

    /// Set (`Some`) or clear (`None`) the liquidation incentive for one
    /// collateral asset (admin only). Same bounds as the global incentive.
    pub fn set_asset_liq_incentive_bps(
        env: Env,
        admin: Address,
        collateral_asset: Address,
        bps: Option<i128>,
    ) -> Result<(), BorrowError> {
        borrow::set_asset_liquidation_incentive_bps(&env, &admin, &collateral_asset, bps)
    }

    /// Liquidation incentive in effect when `collateral_asset` is seized:
    /// its override if set, otherwise the global incentive.
    pub fn get_asset_liq_incentive_bps(env: Env, collateral_asset: Address) -> i128 {
        borrow::get_asset_liquidation_incentive_bps(&env, &collateral_asset)
    }

    /// Current interest rate model parameters.
    pub fn get_interest_rate_config(env: Env) -> InterestRateConfig {
        interest_rate::get_config(&env)
    }

    /// Update interest rate model parameters (admin only). `None` fields are
    /// left unchanged; every value is validated before anything is written.
    /// Returns the new configuration.
    pub fn set_interest_rate_config(
        env: Env,
        admin: Address,
        update: InterestRateConfigUpdate,
    ) -> Result<InterestRateConfig, BorrowError> {
        let (_, next) = interest_rate::update_config(&env, &admin, update)?;
        Ok(next)
    }

    /// Current borrow rate in basis points per year, from the rate model and
    /// pool utilization.
    pub fn get_borrow_rate_bps(env: Env) -> Result<i128, BorrowError> {
        Ok(interest_rate::borrow_rate_bps(&env)?)
    }

    /// Current supply rate in basis points per year (borrow rate minus spread).
    pub fn get_supply_rate_bps(env: Env) -> Result<i128, BorrowError> {
        Ok(interest_rate::supply_rate_bps(&env)?)
    }

    /// Pool utilization in basis points (total debt / debt ceiling).
    pub fn get_utilization_bps(env: Env) -> Result<i128, BorrowError> {
        Ok(interest_rate::utilization_bps(&env)?)
    }

    /// Initialize deposit settings (admin only)
    pub fn initialize_deposit_settings(
        env: Env,
        deposit_cap: i128,
        min_deposit_amount: i128,
    ) -> Result<(), DepositError> {
        let current_admin = get_borrow_admin(&env).ok_or(DepositError::Unauthorized)?;
        current_admin.require_auth();
        initialize_deposit_logic(&env, deposit_cap, min_deposit_amount)
    }

    /// Set deposit pause state (admin only)
    /// Deprecated: use set_pause instead
    pub fn set_deposit_paused(env: Env, paused: bool) -> Result<(), DepositError> {
        let current_admin = get_borrow_admin(&env).ok_or(DepositError::Unauthorized)?;
        current_admin.require_auth();
        env.storage()
            .persistent()
            .set(&pause::PauseDataKey::State(PauseType::Deposit), &paused);
        Ok(())
    }

    /// Get user's deposit collateral position
    pub fn get_user_collateral_deposit(
        env: Env,
        user: Address,
        asset: Address,
    ) -> DepositCollateral {
        get_deposit_collateral(&env, &user, &asset)
    }

    /// Configure (`Some(cap)`) or clear (`None`) the supply cap for a specific asset (admin only).
    pub fn set_asset_supply_cap(
        env: Env,
        admin: Address,
        asset: Address,
        cap: Option<i128>,
    ) -> Result<(), DepositError> {
        set_asset_supply_cap_logic(&env, &admin, &asset, cap)
    }

    /// Read the configured per-asset supply cap for `asset` (`None` when uncapped at the asset level).
    pub fn get_asset_supply_cap(env: Env, asset: Address) -> Option<i128> {
        get_asset_supply_cap_logic(&env, &asset)
    }

    /// Read the aggregate supplied amount currently held for `asset`.
    pub fn get_asset_total_supply(env: Env, asset: Address) -> i128 {
        get_asset_total_supply_logic(&env, &asset)
    }
    // ═══════════════════════════════════════════════════════════════════
    // Compressed calldata (#1045)
    // ═══════════════════════════════════════════════════════════════════

    /// Execute a compressed multi-operation payload for `user` (single auth,
    /// atomic). Returns the number of operations executed.
    pub fn execute_compressed(
        env: Env,
        user: Address,
        payload: Bytes,
    ) -> Result<u32, CalldataError> {
        calldata::execute(&env, user, payload)
    }

    /// Replace the asset dictionary used to resolve compressed asset indices
    /// (admin only).
    pub fn set_calldata_assets(
        env: Env,
        admin: Address,
        assets: Vec<Address>,
    ) -> Result<(), CalldataError> {
        let current_admin = get_borrow_admin(&env).ok_or(CalldataError::Unauthorized)?;
        if admin != current_admin {
            return Err(CalldataError::Unauthorized);
        }
        admin.require_auth();
        calldata::set_dictionary(&env, &assets)
    }

    /// Current compressed-calldata asset dictionary (index = position).
    pub fn get_calldata_assets(env: Env) -> Vec<Address> {
        calldata::get_dictionary(&env)
    }

    /// Encode operations into the compressed wire format (read-only helper for
    /// clients building payloads via simulation).
    pub fn encode_calldata(env: Env, ops: Vec<CompressedOp>) -> Result<Bytes, CalldataError> {
        calldata::encode(&env, &ops)
    }

    /// Decode a compressed payload without executing it.
    pub fn decode_calldata(env: Env, payload: Bytes) -> Result<Vec<CompressedOp>, CalldataError> {
        calldata::decode(&env, &payload)
    }

    // ═══════════════════════════════════════════════════════════════════
    // Lazy pool state (#1046)
    // ═══════════════════════════════════════════════════════════════════

    /// Snapshot of the lazily-initialised pool fields. Pure read: fields that
    /// have never been written report their defaults and are not allocated.
    pub fn get_pool_state(env: Env) -> PoolStateView {
        lazy::snapshot(&env)
    }

    /// Read a single lazily-initialised pool field (default if never written).
    pub fn get_pool_state_field(env: Env, field: LazyField) -> i128 {
        lazy::get(&env, field)
    }

    /// Materialise every lazy field and fold legacy per-field hot-path entries
    /// into their packed slots (admin only). Idempotent; returns the number of
    /// storage entries that were newly written or migrated.
    pub fn migrate_pool_state(env: Env, admin: Address) -> Result<u32, BorrowError> {
        let current_admin = get_borrow_admin(&env).ok_or(BorrowError::Unauthorized)?;
        if admin != current_admin {
            return Err(BorrowError::Unauthorized);
        }
        admin.require_auth();
        let mut written = lazy::migrate_initialize_all(&env);
        if hot_storage::migrate_deposit_state(&env) {
            written += 1;
        }
        if hot_storage::migrate_borrow_limits(&env) {
            written += 1;
        }
        Ok(written)
    }

    /// Get protocol admin
    pub fn get_admin(env: Env) -> Option<Address> {
        get_borrow_admin(&env)
    }

    /// Execute a flash loan
    pub fn flash_loan(
        env: Env,
        receiver: Address,
        asset: Address,
        amount: i128,
        params: Bytes,
    ) -> Result<(), FlashLoanError> {
        flash_loan_logic(&env, receiver, asset, amount, params)
    }

    /// Set the flash loan fee in basis points (admin only)
    pub fn set_flash_loan_fee_bps(env: Env, fee_bps: i128) -> Result<(), FlashLoanError> {
        let current_admin = get_borrow_admin(&env).ok_or(FlashLoanError::Unauthorized)?;
        current_admin.require_auth();
        set_flash_loan_fee_logic(&env, fee_bps)
    }

    // ───────────────────────────────────────────────────────────────────
    // Issue #1400: Flash loan query & configuration API
    // ───────────────────────────────────────────────────────────────────

    /// Get the current flash loan fee in basis points.
    ///
    /// Default is 9 bps (0.09%). Bots and integrators can read this before
    /// estimating profit on a flash loan opportunity.
    pub fn get_flash_loan_fee_bps(env: Env) -> i128 {
        get_flash_loan_fee_bps_logic(&env)
    }

    /// Configure anti-manipulation security parameters for flash loans.
    ///
    /// Controls maximum loan-to-liquidity ratio, price impact cap, and TWAP
    /// deviation window (admin only).
    pub fn set_flash_loan_security_config(
        env: Env,
        config: FlashLoanSecurityConfig,
    ) -> Result<(), FlashLoanError> {
        let current_admin = get_borrow_admin(&env).ok_or(FlashLoanError::Unauthorized)?;
        current_admin.require_auth();
        set_flash_loan_security_config_logic(&env, config)
    }

    /// Get the current flash loan security configuration.
    pub fn get_flash_loan_security_config(env: Env) -> FlashLoanSecurityConfig {
        get_flash_loan_security_config_logic(&env)
    }

    /// Get the security snapshot recorded for the last flash loan on `asset`.
    ///
    /// Returns `None` if no flash loan has been executed for this asset yet.
    /// Useful for bots and monitoring tools to audit recent loan activity.
    pub fn get_flash_loan_security_snapshot(
        env: Env,
        asset: Address,
    ) -> Option<FlashLoanSecuritySnapshot> {
        get_flash_loan_security_snapshot_logic(&env, &asset)
    }

    /// Returns `true` while a flash loan for `asset` is in-progress.
    ///
    /// Designed for re-entrancy detection in callbacks and external monitors.
    pub fn is_asset_flash_loan_active(env: Env, asset: Address) -> bool {
        is_asset_flash_loan_active_logic(&env, &asset)
    }

    // ───────────────────────────────────────────────────────────────────
    // Issue #1403: Partial liquidation dedicated entry point
    // ───────────────────────────────────────────────────────────────────

    /// Partially liquidate an unhealthy position.
    ///
    /// This is a convenience wrapper around `liquidate` that makes the intent
    /// explicit: the caller provides `repay_amount`, which is automatically
    /// capped to `total_debt * close_factor` so the call will not revert due
    /// to an exceeded close factor. Health is verified post-liquidation and
    /// the function succeeds as long as at least 1 unit of debt is repaid.
    ///
    /// # Arguments
    /// * `liquidator`        – the liquidator's address (must sign)
    /// * `borrower`          – the position to liquidate
    /// * `debt_asset`        – the debt token the liquidator will repay
    /// * `collateral_asset`  – the collateral token to seize as reward
    /// * `repay_amount`      – desired repayment (capped to close-factor max)
    ///
    /// # Returns
    /// `(debt_repaid, collateral_seized)` — actual amounts settled.
    ///
    /// Closes issue #1403.
    pub fn partial_liquidate(
        env: Env,
        liquidator: Address,
        borrower: Address,
        debt_asset: Address,
        collateral_asset: Address,
        repay_amount: i128,
    ) -> Result<LiquidationResult, BorrowError> {
        liquidator.require_auth();
        if is_paused(&env, PauseType::Liquidation) {
            return Err(BorrowError::ProtocolPaused);
        }
        // Delegate to the same core implementation as `liquidate`; the close
        // factor enforcement inside `liquidate::liquidate` already clamps the
        // amount, making partial liquidation safe by construction.
        liquidate::liquidate(
            &env,
            liquidator,
            borrower,
            debt_asset,
            collateral_asset,
            repay_amount,
        )
    }

    /// Withdraw collateral from the protocol
    pub fn withdraw(
        env: Env,
        user: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, WithdrawError> {
        if is_paused(&env, PauseType::Withdraw) {
            return Err(WithdrawError::WithdrawPaused);
        }
        withdraw_logic(&env, user, asset, amount)
    }

    /// Emergency withdraw collateral from the protocol with reduced fees
    pub fn emergency_withdraw(
        env: Env,
        user: Address,
        asset: Address,
        amount: i128,
    ) -> Result<i128, WithdrawError> {
        emergency_withdraw_logic(&env, user, asset, amount)
    }

    /// Set emergency withdrawal limit per tx (admin only)
    pub fn set_emergency_withdraw_limit(env: Env, max_amount: i128) -> Result<(), WithdrawError> {
        let current_admin = get_borrow_admin(&env).ok_or(WithdrawError::Unauthorized)?;
        current_admin.require_auth();
        set_emergency_withdraw_limit_logic(&env, max_amount)
    }

    /// Get total emergency analytics stats (total withdrawn, total fees collected)
    pub fn get_emergency_stats(env: Env) -> (i128, i128) {
        get_emergency_stats_logic(&env)
    }

    /// Sweep an existing dust-sized deposit balance below the withdraw minimum.
    pub fn sweep_deposit_dust(
        env: Env,
        user: Address,
        asset: Address,
    ) -> Result<i128, WithdrawError> {
        if is_paused(&env, PauseType::Withdraw) {
            return Err(WithdrawError::WithdrawPaused);
        }
        sweep_deposit_dust_logic(&env, user, asset)
    }

    /// Initialize withdraw settings (admin only)
    pub fn initialize_withdraw_settings(
        env: Env,
        min_withdraw_amount: i128,
    ) -> Result<(), WithdrawError> {
        let current_admin = get_borrow_admin(&env).ok_or(WithdrawError::Unauthorized)?;
        current_admin.require_auth();
        initialize_withdraw_logic(&env, min_withdraw_amount)
    }

    /// Set withdraw pause state (admin only)
    pub fn set_withdraw_paused(env: Env, paused: bool) -> Result<(), WithdrawError> {
        let current_admin = get_borrow_admin(&env).ok_or(WithdrawError::Unauthorized)?;
        current_admin.require_auth();
        set_withdraw_paused_logic(&env, paused)
    }

    /// Token receiver hook
    pub fn receive(
        env: Env,
        token_asset: Address,
        from: Address,
        amount: i128,
        payload: Vec<Val>,
    ) -> Result<(), BorrowError> {
        receive_logic(env, token_asset, from, amount, payload)
    }

    /// Initialize borrow settings (admin only)
    pub fn initialize_borrow_settings(
        env: Env,
        debt_ceiling: i128,
        min_borrow_amount: i128,
    ) -> Result<(), BorrowError> {
        let current_admin = get_borrow_admin(&env).ok_or(BorrowError::Unauthorized)?;
        current_admin.require_auth();
        initialize_borrow_logic(&env, debt_ceiling, min_borrow_amount)
    }

    // ═══════════════════════════════════════════════════════════════════
    // Insurance pool
    // ═══════════════════════════════════════════════════════════════════

    /// Initialize the insurance pool (admin only, call once).
    ///
    /// `admin` must be the protocol admin, so the insurance admin role cannot
    /// be claimed by whoever calls this first after deployment.
    pub fn insurance_initialize(env: Env, admin: Address) -> Result<(), InsuranceError> {
        let current_admin = get_borrow_admin(&env).ok_or(InsuranceError::Unauthorized)?;
        if admin != current_admin {
            return Err(InsuranceError::Unauthorized);
        }
        insurance_initialize(&env, &admin)
    }

    /// Contribute protocol fees to the insurance pool (insurance admin only).
    pub fn insurance_fund_pool(env: Env, amount: i128) -> Result<(), InsuranceError> {
        insurance_fund_pool(&env, amount)
    }

    /// Collect a coverage premium from a user for a given asset.
    /// Returns the premium amount charged.
    pub fn insurance_collect_premium(
        env: Env,
        payer: Address,
        asset: Address,
        coverage_amount: i128,
    ) -> Result<i128, InsuranceError> {
        insurance_collect_premium(&env, payer, asset, coverage_amount)
    }

    /// Submit an insurance claim. Returns the new claim ID.
    pub fn insurance_submit_claim(
        env: Env,
        claimant: Address,
        asset: Address,
        amount: i128,
    ) -> Result<u64, InsuranceError> {
        insurance_submit_claim(&env, claimant, asset, amount)
    }

    /// Evaluate (approve or reject) a pending claim (admin only).
    pub fn insurance_evaluate_claim(
        env: Env,
        admin: Address,
        claim_id: u64,
        approve: bool,
    ) -> Result<(), InsuranceError> {
        insurance_evaluate_claim(&env, admin, claim_id, approve)
    }

    /// Set per-asset coverage limit in basis points (admin only).
    pub fn insurance_set_coverage_limit(
        env: Env,
        admin: Address,
        asset: Address,
        limit_bps: i128,
    ) -> Result<(), InsuranceError> {
        insurance_set_coverage_limit(&env, admin, asset, limit_bps)
    }

    /// Get a claim by ID.
    pub fn insurance_get_claim(env: Env, claim_id: u64) -> Option<InsuranceClaim> {
        insurance_get_claim(&env, claim_id)
    }

    /// Get current dynamic premium rate for an asset (basis points).
    pub fn insurance_get_premium_rate(env: Env, asset: Address) -> i128 {
        insurance_get_premium_rate(&env, &asset)
    }

    /// Get per-asset coverage limit in basis points.
    pub fn insurance_get_coverage_limit(env: Env, asset: Address) -> i128 {
        insurance_get_coverage_limit(&env, &asset)
    }

    /// Get insurance pool analytics.
    pub fn insurance_get_analytics(env: Env) -> InsuranceAnalytics {
        insurance_get_analytics(&env)
    }

    /// Cancel a pending claim (claimant only).
    pub fn insurance_cancel_claim(
        env: Env,
        claimant: Address,
        claim_id: u64,
    ) -> Result<(), InsuranceError> {
        insurance_cancel_claim(&env, claimant, claim_id)
    }

    /// Get all claim IDs for history iteration.
    pub fn insurance_get_all_claim_ids(env: Env) -> Vec<u64> {
        insurance_get_all_claim_ids(&env)
    }

    /// Get all claims (full history).
    pub fn insurance_get_all_claims(env: Env) -> Vec<InsuranceClaim> {
        insurance_get_all_claims(&env)
    }
}
