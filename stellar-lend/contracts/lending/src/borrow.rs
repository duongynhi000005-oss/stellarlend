//! # Borrow Implementation (Simplified Lending)
//!
//! Core borrow logic for the simplified lending contract. Handles collateral
//! validation, debt tracking, interest calculation, and pause controls.
//!
//! ## Interest Model
//! Uses a fixed 5% APY simple interest model:
//! `interest = principal * 500bps * time_elapsed / seconds_per_year`
//!
//! ## Collateral Requirements
//! Minimum collateral ratio is 150% (15,000 basis points).

pub use crate::events::{BorrowCollateralDepositEvent, BorrowEvent, RepayEvent};

/// Backward-compatible name for collateral added to a borrow position (see [`BorrowCollateralDepositEvent`]).
pub type DepositEvent = BorrowCollateralDepositEvent;

use crate::pause::{self, PauseType};
use crate::reentrancy::ReentrancyGuard;
use soroban_sdk::{contracterror, contracttype, Address, Env, IntoVal, Map, Symbol, I256};

#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum RateType {
    Variable = 0,
    Stable = 1,
}

/// Errors that can occur during borrow operations.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum BorrowError {
    /// Collateral amount does not meet the 150% minimum ratio
    InsufficientCollateral = 1,
    /// Total protocol debt would exceed the configured debt ceiling
    DebtCeilingReached = 2,
    /// Borrow operations are currently paused
    ProtocolPaused = 3,
    /// Borrow or collateral amount is zero or negative
    InvalidAmount = 4,
    /// Arithmetic overflow during calculation
    Overflow = 5,
    /// Caller is not authorized for this operation
    Unauthorized = 6,
    /// The requested asset is not supported for borrowing
    AssetNotSupported = 7,
    /// Borrow amount is below the configured minimum
    BelowMinimumBorrow = 8,
    /// Repay amount exceeds current debt
    RepayAmountTooHigh = 9,
    /// Position is healthy and cannot be liquidated
    PositionHealthy = 10,
    /// Insufficient reserves to recover bad debt
    InsufficientReserves = 11,
    /// Reentrant call detected
    ReentrancyDetected = 12,
    /// Utilization circuit breaker is throttling new borrows
    UtilizationThrottled = 13,
    /// Position already holds `MAX_COLLATERAL_ASSETS` distinct collateral assets
    TooManyCollateralAssets = 14,
    /// A borrower cannot liquidate their own position
    SelfLiquidation = 15,
    /// The oracle has no usable price for an asset the operation depends on
    PriceUnavailable = 16,
    /// Liquidation incentive is outside the allowed bounds
    IncentiveOutOfBounds = 17,
    /// Operation violates isolated risk pool boundaries (collateral and debt must belong to the same pool)
    IsolatedPoolMismatch = 18,
}

/// Borrow on behalf of a user when authorization is provided via a trusted delegate.
///
/// This bypasses `user.require_auth()` and is intended to be called only after
/// delegation/nonce checks at a higher layer.
pub(crate) fn borrow_trusted(
    env: &Env,
    user: Address,
    asset: Address,
    amount: i128,
    collateral_asset: Address,
    collateral_amount: i128,
) -> Result<(), BorrowError> {
    borrow_inner(
        env,
        user,
        asset,
        amount,
        collateral_asset,
        collateral_amount,
        RateType::Variable,
        BorrowAuth::TrustedCommitment,
    )
}

/// Storage keys for protocol-wide data.
#[contracttype]
#[derive(Clone)]
#[allow(clippy::enum_variant_names)]
pub enum BorrowDataKey {
    /// Protocol admin address
    ProtocolAdmin,
    /// Per-user debt position
    BorrowUserDebt(Address),
    /// Per-user variable-rate debt position
    BorrowUserVariableDebt(Address),
    /// Per-user stable-rate debt position
    BorrowUserStableDebt(Address),
    /// Per-user collateral position
    BorrowUserCollateral(Address),
    /// Aggregate protocol debt
    BorrowTotalDebt,
    /// Maximum total debt allowed (legacy; now packed in `HotStorageKey::BorrowLimits`)
    BorrowDebtCeiling,
    /// Interest rate configuration
    BorrowInterestRate,
    /// Collateral ratio configuration
    BorrowCollateralRatio,
    /// Minimum borrow amount (legacy; now packed in `HotStorageKey::BorrowLimits`)
    BorrowMinAmount,
    /// Oracle contract address for price feeds (optional)
    OracleAddress,
    /// Liquidation threshold in basis points (e.g. 8000 = 80%)
    LiquidationThresholdBps,
    /// Close factor in basis points (e.g. 5000 = 50%)
    CloseFactorBps,
    /// Liquidation incentive in basis points (e.g. 1000 = 10%)
    LiquidationIncentiveBps,
    /// Global interest index (for invariant testing)
    InterestIndex,
    /// Stablecoin configuration for a specific asset
    AssetStablecoinConfig(Address),

    /// Stable borrow rate state (protocol-wide)
    StableRateState,
    /// Stable rate premium in basis points
    StableRatePremiumBps,
    /// Stable rate recalculation interval in seconds
    StableRateRecalcIntervalSecs,
    /// Switch fee in basis points applied when switching rate types
    RateSwitchFeeBps,
    /// Transaction-scoped oracle price cache. Never persists across ledgers.
    OraclePriceCache(Address),
    /// Per-user collateral balances, keyed by asset. Supersedes the
    /// single-asset `BorrowUserCollateral` record, which is migrated on the
    /// first write.
    BorrowUserCollaterals(Address),
    /// Per-asset liquidation incentive override in basis points
    AssetLiquidationIncentiveBps(Address),
    /// Risk pool ID an asset is assigned to (0 = shared core pool, >0 = isolated pool)
    AssetPool(Address),
    /// Configured aggregate debt ceiling for an isolated risk pool ID
    PoolDebtCeiling(u32),
    /// Aggregate active debt tracked within an isolated risk pool ID
    PoolTotalDebt(u32),
}

/// Dynamic stablecoin configuration.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StablecoinConfig {
    /// Target price in oracle units (e.g. 100_000_000 for $1.00)
    pub target_price: i128,
    /// Minimum deviation before stability fee kicks in (basis points)
    pub peg_threshold_bps: i128,
    /// Fee added to interest rate when depegged (basis points)
    pub stability_fee_bps: i128,
    /// Threshold for emergency actions (basis points)
    pub emergency_threshold_bps: i128,
}

/// User debt position tracking.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DebtPosition {
    /// Principal amount borrowed
    pub borrowed_amount: i128,
    /// Cumulative interest accrued
    pub interest_accrued: i128,
    /// Timestamp of last interest accrual
    pub last_update: u64,
    /// Address of the borrowed asset
    pub asset: Address,

    pub rate_type: RateType,
    pub stable_rate_bps: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StableRateState {
    pub avg_rate_bps: i128,
    pub last_update: u64,
}

/// User collateral position tracking.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BorrowCollateral {
    /// Amount of collateral deposited
    pub amount: i128,
    /// Address of the collateral asset
    pub asset: Address,
}

const COLLATERAL_RATIO_MIN: i128 = 15000; // 150% in basis points

/// Most distinct collateral assets one position may hold. Bounds the oracle
/// calls a health-factor check or liquidation makes per position.
pub const MAX_COLLATERAL_ASSETS: u32 = 8;

/// Lowest liquidation incentive (1%): below this, liquidating is rarely worth
/// a liquidator's gas, so unhealthy positions would linger.
pub const MIN_LIQUIDATION_INCENTIVE_BPS: i128 = 100;
/// Highest liquidation incentive (25%). Together with the default 80%
/// liquidation threshold this keeps `threshold * (1 + incentive) <= 100%`,
/// so a liquidation never seizes more value than backs the debt it repays.
pub const MAX_LIQUIDATION_INCENTIVE_BPS: i128 = 2500;
/// Incentive used when neither a per-asset nor a global value is set.
pub const DEFAULT_LIQUIDATION_INCENTIVE_BPS: i128 = 1000;
const SECONDS_PER_YEAR: u64 = 31536000;

const DEFAULT_STABLE_PREMIUM_BPS: i128 = 100; // 1%
const DEFAULT_STABLE_RECALC_INTERVAL_SECS: u64 = 6 * 3600; // 6h
const DEFAULT_SWITCH_FEE_BPS: i128 = 10; // 0.10%

fn get_current_variable_rate_bps(env: &Env) -> Result<i128, BorrowError> {
    crate::interest_rate::borrow_rate_bps(env).map_err(|e| {
        let be: BorrowError = e.into();
        be
    })
}

pub fn set_variable_borrow_rate_bps(
    env: &Env,
    admin: &Address,
    rate_bps: i128,
) -> Result<(), BorrowError> {
    let current = get_admin(env).ok_or(BorrowError::Unauthorized)?;
    if *admin != current {
        return Err(BorrowError::Unauthorized);
    }
    admin.require_auth();
    if !(0..=10000).contains(&rate_bps) {
        return Err(BorrowError::InvalidAmount);
    }

    let update = crate::interest_rate::InterestRateConfigUpdate {
        base_rate_bps: Some(rate_bps),
        kink_utilization_bps: None,
        slope_bps: None,
        jump_slope_bps: None,
        rate_floor_bps: None,
        rate_ceiling_bps: None,
        spread_bps: None,
    };

    crate::interest_rate::update_config(env, admin, update).map_err(|e| {
        let be: BorrowError = e.into();
        be
    })?;
    Ok(())
}

fn get_stable_rate_premium_bps(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::StableRatePremiumBps)
        .unwrap_or(DEFAULT_STABLE_PREMIUM_BPS)
}

fn get_stable_rate_recalc_interval_secs(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::StableRateRecalcIntervalSecs)
        .unwrap_or(DEFAULT_STABLE_RECALC_INTERVAL_SECS)
}

fn get_rate_switch_fee_bps(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::RateSwitchFeeBps)
        .unwrap_or(DEFAULT_SWITCH_FEE_BPS)
}

fn get_stable_rate_state(env: &Env) -> StableRateState {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::StableRateState)
        .unwrap_or(StableRateState {
            avg_rate_bps: get_current_variable_rate_bps(env).unwrap_or(0),
            last_update: env.ledger().timestamp(),
        })
}

fn update_stable_rate_state_if_needed(env: &Env) -> StableRateState {
    let mut state = get_stable_rate_state(env);
    let now = env.ledger().timestamp();
    let interval = get_stable_rate_recalc_interval_secs(env);

    if now <= state.last_update {
        return state;
    }

    if interval == 0 || now - state.last_update < interval {
        return state;
    }

    // Simple EMA smoothing toward current variable rate:
    // new_avg = (old * 9 + current) / 10
    let current = match get_current_variable_rate_bps(env) {
        Ok(v) => v,
        Err(_) => state.avg_rate_bps,
    };
    let new_avg = state
        .avg_rate_bps
        .checked_mul(9)
        .and_then(|v| v.checked_add(current))
        .and_then(|v| v.checked_div(10))
        .unwrap_or(current);

    state.avg_rate_bps = new_avg;
    state.last_update = now;

    env.storage()
        .persistent()
        .set(&BorrowDataKey::StableRateState, &state);

    state
}

fn get_current_stable_rate_bps(env: &Env) -> Result<i128, BorrowError> {
    let state = update_stable_rate_state_if_needed(env);
    let premium = get_stable_rate_premium_bps(env);
    state
        .avg_rate_bps
        .checked_add(premium)
        .ok_or(BorrowError::Overflow)
}

/// Borrow assets against deposited collateral (requires direct user authorization).
pub fn borrow(
    env: &Env,
    user: Address,
    asset: Address,
    amount: i128,
    collateral_asset: Address,
    collateral_amount: i128,
) -> Result<(), BorrowError> {
    borrow_with_rate(
        env,
        user,
        asset,
        amount,
        collateral_asset,
        collateral_amount,
        RateType::Variable,
    )
}

pub fn borrow_with_rate(
    env: &Env,
    user: Address,
    asset: Address,
    amount: i128,
    collateral_asset: Address,
    collateral_amount: i128,
    rate_type: RateType,
) -> Result<(), BorrowError> {
    borrow_inner(
        env,
        user,
        asset,
        amount,
        collateral_asset,
        collateral_amount,
        rate_type,
        BorrowAuth::RequireUserSignature,
    )
}

/// Borrow on behalf of a user who previously authorized a scheduled commitment (no `user.require_auth`).
pub(crate) fn borrow_from_commitment(
    env: &Env,
    user: Address,
    asset: Address,
    amount: i128,
    collateral_asset: Address,
    collateral_amount: i128,
) -> Result<(), BorrowError> {
    borrow_inner(
        env,
        user,
        asset,
        amount,
        collateral_asset,
        collateral_amount,
        RateType::Variable,
        BorrowAuth::TrustedCommitment,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorrowAuth {
    RequireUserSignature,
    TrustedCommitment,
}

fn borrow_inner(
    env: &Env,
    user: Address,
    asset: Address,
    amount: i128,
    collateral_asset: Address,
    collateral_amount: i128,
    rate_type: RateType,
    auth: BorrowAuth,
) -> Result<(), BorrowError> {
    let _guard = ReentrancyGuard::new(env).map_err(|_| BorrowError::ReentrancyDetected)?;

    if auth == BorrowAuth::RequireUserSignature {
        user.require_auth();
    }

    if pause::is_paused(env, PauseType::Borrow) {
        return Err(BorrowError::ProtocolPaused);
    }

    if amount <= 0 || collateral_amount <= 0 {
        return Err(BorrowError::InvalidAmount);
    }

    // Both static limits come from one packed entry (#1043).
    let limits = crate::hot_storage::borrow_limits(env);
    if amount < limits.min_borrow {
        return Err(BorrowError::BelowMinimumBorrow);
    }

    validate_collateral_ratio(collateral_amount, amount)?;

    let debt_pool = get_asset_pool(env, &asset);
    let collateral_pool = get_asset_pool(env, &collateral_asset);
    if debt_pool != collateral_pool {
        return Err(BorrowError::IsolatedPoolMismatch);
    }

    if debt_pool > 0 {
        let pool_ceiling = get_pool_debt_ceiling(env, debt_pool);
        let pool_total = get_pool_total_debt(env, debt_pool);
        let new_pool_total = pool_total
            .checked_add(amount)
            .ok_or(BorrowError::Overflow)?;
        if pool_ceiling > 0 && new_pool_total > pool_ceiling {
            return Err(BorrowError::DebtCeilingReached);
        }
        set_pool_total_debt(env, debt_pool, new_pool_total);
    }

    let total_debt = get_total_debt(env);
    let new_total = total_debt
        .checked_add(amount)
        .ok_or(BorrowError::Overflow)?;

    if new_total > limits.debt_ceiling {
        return Err(BorrowError::DebtCeilingReached);
    }

    crate::utilization_breaker::check_borrow(env, amount, new_total, limits.debt_ceiling)?;

    let mut debt_position = get_debt_position(env, &user, Some(&asset), rate_type);
    debt_position.rate_type = rate_type;
    let accrued_interest = calculate_interest(env, &debt_position)?;

    debt_position.borrowed_amount = debt_position
        .borrowed_amount
        .checked_add(amount)
        .ok_or(BorrowError::Overflow)?;
    debt_position.interest_accrued = debt_position
        .interest_accrued
        .checked_add(accrued_interest)
        .ok_or(BorrowError::Overflow)?;
    debt_position.last_update = env.ledger().timestamp();
    debt_position.asset = asset.clone();

    if rate_type == RateType::Stable {
        debt_position.stable_rate_bps = get_current_stable_rate_bps(env)?;
    }

    save_debt_position(env, &user, &debt_position);
    add_collateral(env, &user, &collateral_asset, collateral_amount)?;
    set_total_debt(env, new_total);

    // Lazy pool state (#1046): the borrow-index snapshot is only materialised
    // by the pool's first borrow, never at pool creation.
    let snapshot = crate::lazy::LazyField::BorrowIndexSnapshot;
    if total_debt == 0 && !crate::lazy::is_initialized(env, snapshot) {
        crate::lazy::set(env, snapshot, get_interest_index(env))
            .map_err(|_| BorrowError::Overflow)?;
    }

    crate::risk_monitor::on_utilization_changed(env, new_total, limits.debt_ceiling);
    crate::utilization_breaker::on_debt_changed(env, new_total, limits.debt_ceiling);

    emit_borrow_event(env, user, asset, amount);

    Ok(())
}

/// Deposit collateral
///
/// # Arguments
/// * `env` - The contract environment
/// * `user` - The user's address
/// * `asset` - The collateral asset
/// * `amount` - The amount to deposit
pub fn deposit(env: &Env, user: Address, asset: Address, amount: i128) -> Result<(), BorrowError> {
    if amount <= 0 {
        return Err(BorrowError::InvalidAmount);
    }

    add_collateral(env, &user, &asset, amount)?;

    BorrowCollateralDepositEvent {
        user,
        asset,
        amount,
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);

    Ok(())
}

/// Repay borrowed assets
///
/// # Arguments
/// * `env` - The contract environment
/// * `user` - The user's address
/// * `asset` - The borrowed asset
/// * `amount` - The amount to repay
pub fn repay(env: &Env, user: Address, asset: Address, amount: i128) -> Result<(), BorrowError> {
    let variable = get_debt_position(env, &user, Some(&asset), RateType::Variable);
    if variable.borrowed_amount > 0 || variable.interest_accrued > 0 {
        repay_with_rate(env, user, asset, amount, RateType::Variable)
    } else {
        repay_with_rate(env, user, asset, amount, RateType::Stable)
    }
}

pub fn repay_with_rate(
    env: &Env,
    user: Address,
    asset: Address,
    amount: i128,
    rate_type: RateType,
) -> Result<(), BorrowError> {
    let _guard = ReentrancyGuard::new(env).map_err(|_| BorrowError::ReentrancyDetected)?;

    if amount <= 0 {
        return Err(BorrowError::InvalidAmount);
    }

    let mut debt_position = get_debt_position(env, &user, Some(&asset), rate_type);
    debt_position.rate_type = rate_type;

    if debt_position.borrowed_amount == 0 && debt_position.interest_accrued == 0 {
        return Err(BorrowError::InvalidAmount);
    }

    if debt_position.asset != asset {
        return Err(BorrowError::AssetNotSupported);
    }

    // First repay interest, then principal
    let accrued_interest = calculate_interest(env, &debt_position)?;
    debt_position.interest_accrued = debt_position
        .interest_accrued
        .checked_add(accrued_interest)
        .ok_or(BorrowError::Overflow)?;
    debt_position.last_update = env.ledger().timestamp();

    let mut remaining_repayment = amount;

    // Repay interest first
    if remaining_repayment >= debt_position.interest_accrued {
        remaining_repayment -= debt_position.interest_accrued;
        debt_position.interest_accrued = 0;
    } else {
        debt_position.interest_accrued -= remaining_repayment;
        remaining_repayment = 0;
    }

    // Repay principal
    if remaining_repayment > 0 {
        if remaining_repayment > debt_position.borrowed_amount {
            return Err(BorrowError::RepayAmountTooHigh);
        }
        debt_position.borrowed_amount -= remaining_repayment;

        // Update total protocol debt
        let total_debt = get_total_debt(env);
        let new_total = total_debt
            .checked_sub(remaining_repayment)
            .ok_or(BorrowError::Overflow)?;
        set_total_debt(env, new_total);

        let debt_pool = get_asset_pool(env, &asset);
        decrement_pool_total_debt(env, debt_pool, remaining_repayment);
        crate::utilization_breaker::on_debt_changed(env, new_total, get_debt_ceiling(env));
    }

    save_debt_position(env, &user, &debt_position);

    RepayEvent {
        user,
        asset: Some(asset),
        amount,
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);

    Ok(())
}

pub fn switch_rate_type(
    env: &Env,
    user: Address,
    asset: Address,
    to_rate_type: RateType,
) -> Result<(), BorrowError> {
    user.require_auth();

    let from_rate_type = if to_rate_type == RateType::Variable {
        RateType::Stable
    } else {
        RateType::Variable
    };

    let mut from_position = get_debt_position(env, &user, Some(&asset), from_rate_type);
    from_position.rate_type = from_rate_type;
    if from_position.borrowed_amount == 0 && from_position.interest_accrued == 0 {
        return Err(BorrowError::InvalidAmount);
    }

    let mut to_position = get_debt_position(env, &user, Some(&asset), to_rate_type);
    to_position.rate_type = to_rate_type;
    if to_position.borrowed_amount != 0 || to_position.interest_accrued != 0 {
        return Err(BorrowError::AssetNotSupported);
    }

    // Accrue interest on the source position before moving.
    let accrued_interest = calculate_interest(env, &from_position)?;
    from_position.interest_accrued = from_position
        .interest_accrued
        .checked_add(accrued_interest)
        .ok_or(BorrowError::Overflow)?;
    from_position.last_update = env.ledger().timestamp();

    // Switch fee as an added cost on the source principal.
    let fee_bps = get_rate_switch_fee_bps(env);
    let fee = from_position
        .borrowed_amount
        .checked_mul(fee_bps)
        .ok_or(BorrowError::Overflow)?
        .checked_div(10000)
        .ok_or(BorrowError::Overflow)?;
    from_position.interest_accrued = from_position
        .interest_accrued
        .checked_add(fee)
        .ok_or(BorrowError::Overflow)?;

    // Move into the target bucket.
    to_position.borrowed_amount = from_position.borrowed_amount;
    to_position.interest_accrued = from_position.interest_accrued;
    to_position.last_update = env.ledger().timestamp();
    to_position.asset = asset;

    if to_rate_type == RateType::Stable {
        to_position.stable_rate_bps = get_current_stable_rate_bps(env)?;
    }

    // Clear the source bucket.
    from_position.borrowed_amount = 0;
    from_position.interest_accrued = 0;
    from_position.last_update = env.ledger().timestamp();
    from_position.stable_rate_bps = 0;

    save_debt_position(env, &user, &from_position);
    save_debt_position(env, &user, &to_position);

    Ok(())
}

/// Validate collateral ratio meets minimum requirements
pub(crate) fn validate_collateral_ratio(collateral: i128, borrow: i128) -> Result<(), BorrowError> {
    let min_collateral = borrow
        .checked_mul(COLLATERAL_RATIO_MIN)
        .ok_or(BorrowError::Overflow)?
        .checked_div(10000)
        .ok_or(BorrowError::InvalidAmount)?;

    if collateral < min_collateral {
        return Err(BorrowError::InsufficientCollateral);
    }

    Ok(())
}

pub(crate) fn calculate_interest(env: &Env, position: &DebtPosition) -> Result<i128, BorrowError> {
    if position.borrowed_amount == 0 {
        return Ok(0);
    }

    let current_time = env.ledger().timestamp();
    let time_elapsed = current_time.saturating_sub(position.last_update);

    let borrowed_256 = I256::from_i128(env, position.borrowed_amount);
    let rate_bps = match position.rate_type {
        RateType::Variable => get_current_variable_rate_bps(env)?,
        RateType::Stable => {
            if position.stable_rate_bps > 0 {
                position.stable_rate_bps
            } else {
                get_current_stable_rate_bps(env)?
            }
        }
    };
    let rate_256 = I256::from_i128(env, rate_bps);
    let time_256 = I256::from_i128(env, time_elapsed as i128);

    let mut interest_256 = borrowed_256
        .mul(&rate_256)
        .mul(&time_256)
        .div(&I256::from_i128(env, 10000))
        .div(&I256::from_i128(env, SECONDS_PER_YEAR as i128));

    // Stability fee logic
    if let Some(config) = get_stablecoin_config(env, &position.asset) {
        if let Some(oracle) = get_oracle(env) {
            let price = get_asset_price(env, &oracle, &position.asset);
            let deviation = config.target_price.saturating_sub(price);
            let deviation_bps = if config.target_price > 0 {
                deviation
                    .saturating_mul(10000)
                    .saturating_div(config.target_price)
            } else {
                0
            };

            if deviation_bps > config.peg_threshold_bps {
                let stability_fee_256 = borrowed_256
                    .mul(&I256::from_i128(env, config.stability_fee_bps))
                    .mul(&time_256)
                    .div(&I256::from_i128(env, 10000))
                    .div(&I256::from_i128(env, SECONDS_PER_YEAR as i128));

                interest_256 = interest_256.add(&stability_fee_256);

                crate::events::PegDeviationEvent {
                    asset: position.asset.clone(),
                    price,
                    target_price: config.target_price,
                    deviation_bps,
                    timestamp: env.ledger().timestamp(),
                }
                .publish(env);

                crate::events::StabilityFeeAppliedEvent {
                    asset: position.asset.clone(),
                    fee_bps: config.stability_fee_bps,
                    timestamp: env.ledger().timestamp(),
                }
                .publish(env);
            }
        }
    }

    interest_256.to_i128().ok_or(BorrowError::Overflow)
}

pub(crate) fn get_asset_price(env: &Env, oracle: &Address, asset: &Address) -> i128 {
    let cache_key = BorrowDataKey::OraclePriceCache(asset.clone());
    let now = env.ledger().timestamp();
    if let Some((cached_at, cached_price)) =
        env.storage().temporary().get::<_, (u64, i128)>(&cache_key)
    {
        if cached_at == now {
            return cached_price;
        }
    }

    let price: i128 = env.invoke_contract(
        oracle,
        &Symbol::new(env, "price"),
        (asset.clone(),).into_val(env),
    );
    env.storage().temporary().set(&cache_key, &(now, price));
    price
}

pub(crate) fn get_debt_position(
    env: &Env,
    user: &Address,
    default_asset: Option<&Address>,
    rate_type: RateType,
) -> DebtPosition {
    let key = match rate_type {
        RateType::Variable => BorrowDataKey::BorrowUserVariableDebt(user.clone()),
        RateType::Stable => BorrowDataKey::BorrowUserStableDebt(user.clone()),
    };

    // Backward compat: if new key doesn't exist, fall back to legacy BorrowUserDebt
    // for variable-rate position.
    if !env.storage().persistent().has(&key) && rate_type == RateType::Variable {
        if let Some(legacy) = env
            .storage()
            .persistent()
            .get::<BorrowDataKey, DebtPosition>(&BorrowDataKey::BorrowUserDebt(user.clone()))
        {
            return DebtPosition {
                borrowed_amount: legacy.borrowed_amount,
                interest_accrued: legacy.interest_accrued,
                last_update: legacy.last_update,
                asset: legacy.asset,
                rate_type: RateType::Variable,
                stable_rate_bps: 0,
            };
        }
    }

    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(DebtPosition {
            borrowed_amount: 0,
            interest_accrued: 0,
            last_update: env.ledger().timestamp(),
            asset: default_asset.cloned().unwrap_or_else(|| user.clone()),
            rate_type,
            stable_rate_bps: 0,
        })
}

pub(crate) fn save_debt_position(env: &Env, user: &Address, position: &DebtPosition) {
    let key = match position.rate_type {
        RateType::Variable => BorrowDataKey::BorrowUserVariableDebt(user.clone()),
        RateType::Stable => BorrowDataKey::BorrowUserStableDebt(user.clone()),
    };

    env.storage().persistent().set(&key, position);
    update_compat_user_debt(env, user);
}

fn update_compat_user_debt(env: &Env, user: &Address) {
    // New positions use the variable/stable keys directly. Keep writing the
    // legacy compatibility key only when it already exists, so new positions
    // avoid a redundant persistent write while old deployments remain readable.
    let legacy_key = BorrowDataKey::BorrowUserDebt(user.clone());
    if !env.storage().persistent().has(&legacy_key) {
        return;
    }

    let v = env
        .storage()
        .persistent()
        .get::<BorrowDataKey, DebtPosition>(&BorrowDataKey::BorrowUserVariableDebt(user.clone()));
    let s = env
        .storage()
        .persistent()
        .get::<BorrowDataKey, DebtPosition>(&BorrowDataKey::BorrowUserStableDebt(user.clone()));

    let chosen = if let Some(pos) = v {
        if pos.borrowed_amount > 0 || pos.interest_accrued > 0 {
            Some(pos)
        } else {
            s
        }
    } else {
        s
    };

    if let Some(pos) = chosen {
        env.storage().persistent().set(&legacy_key, &pos);
    } else {
        // Ensure legacy key is cleared to an empty variable position
        env.storage().persistent().set(
            &legacy_key,
            &DebtPosition {
                borrowed_amount: 0,
                interest_accrued: 0,
                last_update: env.ledger().timestamp(),
                asset: user.clone(),
                rate_type: RateType::Variable,
                stable_rate_bps: 0,
            },
        );
    }
}

/// Collateral balances of `user`, keyed by asset. Positions written before
/// multi-collateral support are read from the legacy single-asset record.
pub fn get_collateral_balances(env: &Env, user: &Address) -> Map<Address, i128> {
    if let Some(balances) = env
        .storage()
        .persistent()
        .get(&BorrowDataKey::BorrowUserCollaterals(user.clone()))
    {
        return balances;
    }

    let mut balances = Map::new(env);
    if let Some(legacy) = env
        .storage()
        .persistent()
        .get::<_, BorrowCollateral>(&BorrowDataKey::BorrowUserCollateral(user.clone()))
    {
        if legacy.amount > 0 {
            balances.set(legacy.asset, legacy.amount);
        }
    }
    balances
}

fn save_collateral_balances(env: &Env, user: &Address, balances: &Map<Address, i128>) {
    env.storage().persistent().set(
        &BorrowDataKey::BorrowUserCollaterals(user.clone()),
        balances,
    );
    // The map now holds everything the legacy record did.
    env.storage()
        .persistent()
        .remove(&BorrowDataKey::BorrowUserCollateral(user.clone()));
}

/// Credit `amount` of `asset` to `user`'s collateral. Returns the new balance.
pub(crate) fn add_collateral(
    env: &Env,
    user: &Address,
    asset: &Address,
    amount: i128,
) -> Result<i128, BorrowError> {
    let mut balances = get_collateral_balances(env, user);
    let current = balances.get(asset.clone()).unwrap_or(0);
    if current == 0 && balances.len() >= MAX_COLLATERAL_ASSETS {
        return Err(BorrowError::TooManyCollateralAssets);
    }
    let updated = current.checked_add(amount).ok_or(BorrowError::Overflow)?;
    balances.set(asset.clone(), updated);
    save_collateral_balances(env, user, &balances);
    Ok(updated)
}

/// Debit `amount` of `asset` from `user`'s collateral, dropping the asset
/// once its balance reaches zero. Returns the new balance.
pub(crate) fn remove_collateral(
    env: &Env,
    user: &Address,
    asset: &Address,
    amount: i128,
) -> Result<i128, BorrowError> {
    let mut balances = get_collateral_balances(env, user);
    debit_collateral_balances(env, user, &mut balances, asset, amount)
}

/// Same as [`remove_collateral`] for a balance map the caller already loaded
/// with [`get_collateral_balances`], so the record is not read twice in one
/// call. `balances` must be the current state of `user`'s collateral.
pub(crate) fn debit_collateral_balances(
    env: &Env,
    user: &Address,
    balances: &mut Map<Address, i128>,
    asset: &Address,
    amount: i128,
) -> Result<i128, BorrowError> {
    let current = balances.get(asset.clone()).unwrap_or(0);
    if amount > current {
        return Err(BorrowError::InsufficientCollateral);
    }
    let updated = current - amount;
    if updated == 0 {
        balances.remove(asset.clone());
    } else {
        balances.set(asset.clone(), updated);
    }
    save_collateral_balances(env, user, balances);
    Ok(updated)
}

/// Single-asset view of a position, kept for callers that predate
/// multi-collateral support: the asset with the largest raw balance. Use
/// [`get_collateral_balances`] to see every asset.
fn get_collateral_position(env: &Env, user: &Address) -> BorrowCollateral {
    let mut primary = BorrowCollateral {
        amount: 0,
        asset: user.clone(),
    };
    for (asset, amount) in get_collateral_balances(env, user).iter() {
        if amount > primary.amount {
            primary = BorrowCollateral { amount, asset };
        }
    }
    primary
}

pub(crate) fn get_total_debt(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::BorrowTotalDebt)
        .unwrap_or(0)
}

pub(crate) fn set_total_debt(env: &Env, amount: i128) {
    env.storage()
        .persistent()
        .set(&BorrowDataKey::BorrowTotalDebt, &amount);
}

/// Zero out a borrow position whose remaining principal is below the configured minimum borrow
/// amount (economically negligible "dust"), crediting the swept amount back to the protocol's
/// total debt. Returns the swept amount, or `0` when there is nothing to sweep.
pub fn sweep_debt_dust(env: &Env, user: Address, asset: Address) -> Result<i128, BorrowError> {
    for rate_type in [RateType::Variable, RateType::Stable] {
        let key = match rate_type {
            RateType::Variable => BorrowDataKey::BorrowUserVariableDebt(user.clone()),
            RateType::Stable => BorrowDataKey::BorrowUserStableDebt(user.clone()),
        };
        let Some(mut debt_position) = env.storage().persistent().get::<_, DebtPosition>(&key)
        else {
            continue;
        };

        if debt_position.asset != asset || debt_position.borrowed_amount == 0 {
            continue;
        }

        // Position is only "dust" when below the protocol's minimum borrow amount.
        if debt_position.borrowed_amount >= get_min_borrow_amount(env) {
            return Ok(0);
        }

        let swept = debt_position.borrowed_amount;
        debt_position.borrowed_amount = 0;
        env.storage().persistent().set(&key, &debt_position);
        update_compat_user_debt(env, &user);

        let total_debt = get_total_debt(env);
        let new_total = total_debt.checked_sub(swept).ok_or(BorrowError::Overflow)?;
        set_total_debt(env, new_total);
        crate::utilization_breaker::on_debt_changed(env, new_total, get_debt_ceiling(env));

        let debt_pool = get_asset_pool(env, &asset);
        decrement_pool_total_debt(env, debt_pool, swept);

        return Ok(swept);
    }

    Ok(0)
}

pub(crate) fn get_debt_ceiling(env: &Env) -> i128 {
    crate::hot_storage::borrow_limits(env).debt_ceiling
}

pub(crate) fn get_min_borrow_amount(env: &Env) -> i128 {
    crate::hot_storage::borrow_limits(env).min_borrow
}

fn emit_borrow_event(env: &Env, user: Address, asset: Address, amount: i128) {
    BorrowEvent {
        user,
        asset: Some(asset),
        amount,
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);
}

pub fn initialize_borrow_settings(
    env: &Env,
    debt_ceiling: i128,
    min_borrow_amount: i128,
) -> Result<(), BorrowError> {
    // Note: ProtocolAdmin check should be performed by the caller (lib.rs)
    // A non-positive ceiling would block all borrowing (and zero out the
    // utilization curve); a minimum above the ceiling makes every borrow fail.
    if debt_ceiling <= 0 || min_borrow_amount <= 0 || min_borrow_amount > debt_ceiling {
        return Err(BorrowError::InvalidAmount);
    }
    crate::hot_storage::store_borrow_limits(
        env,
        &crate::hot_storage::BorrowLimits {
            debt_ceiling,
            min_borrow: min_borrow_amount,
        },
    );
    crate::interest_rate::set_default_if_missing(env);
    // Stable-rate premium, recalculation interval and switch fee are *not*
    // written here any more (#1046): their getters already fall back to the
    // `DEFAULT_*` constants, so the slots are only allocated when an admin
    // overrides them. This saves three persistent entries per pool.
    Ok(())
}

pub fn get_user_debt(env: &Env, user: &Address) -> DebtPosition {
    let variable = get_debt_position(env, user, None, RateType::Variable);
    let stable = get_debt_position(env, user, None, RateType::Stable);

    let mut position = if variable.borrowed_amount > 0 || variable.interest_accrued > 0 {
        variable
    } else {
        stable
    };

    match calculate_interest(env, &position) {
        Ok(accrued) => {
            position.interest_accrued = position.interest_accrued.saturating_add(accrued);
        }
        Err(BorrowError::Overflow) => {
            // Read-only view: saturate rather than under-reporting interest.
            position.interest_accrued = i128::MAX;
        }
        Err(_) => {}
    }
    position
}

pub fn get_user_debt_with_rate(env: &Env, user: &Address, rate_type: RateType) -> DebtPosition {
    let mut position = get_debt_position(env, user, None, rate_type);
    match calculate_interest(env, &position) {
        Ok(accrued) => {
            position.interest_accrued = position.interest_accrued.saturating_add(accrued);
        }
        Err(BorrowError::Overflow) => {
            position.interest_accrued = i128::MAX;
        }
        Err(_) => {}
    }
    position
}

pub fn get_user_collateral(env: &Env, user: &Address) -> BorrowCollateral {
    get_collateral_position(env, user)
}

pub fn set_admin(env: &Env, admin: &Address) {
    env.storage()
        .persistent()
        .set(&BorrowDataKey::ProtocolAdmin, admin);
}

pub fn get_admin(env: &Env) -> Option<Address> {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::ProtocolAdmin)
}

/// Returns the oracle address if configured. Used by views for collateral/debt valuation.
pub fn get_oracle(env: &Env) -> Option<Address> {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::OracleAddress)
}

/// Returns liquidation threshold in basis points (e.g. 8000 = 80%). Default 8000 if not set.
pub fn get_liquidation_threshold_bps(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::LiquidationThresholdBps)
        .unwrap_or(8000)
}

/// Returns close factor in basis points (e.g. 5000 = 50%). Default 5000 if not set.
pub fn get_close_factor_bps(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::CloseFactorBps)
        .unwrap_or(5000)
}

/// Returns the global liquidation incentive in basis points (e.g. 1000 = 10%),
/// used for assets without an override. Default 1000 if not set.
pub fn get_liquidation_incentive_bps(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::LiquidationIncentiveBps)
        .unwrap_or(DEFAULT_LIQUIDATION_INCENTIVE_BPS)
}

/// Liquidation incentive applied when `collateral_asset` is seized: the
/// asset's override if one is set, otherwise the global incentive.
pub fn get_asset_liquidation_incentive_bps(env: &Env, collateral_asset: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::AssetLiquidationIncentiveBps(
            collateral_asset.clone(),
        ))
        .unwrap_or_else(|| get_liquidation_incentive_bps(env))
}

fn validate_liquidation_incentive(bps: i128) -> Result<(), BorrowError> {
    if !(MIN_LIQUIDATION_INCENTIVE_BPS..=MAX_LIQUIDATION_INCENTIVE_BPS).contains(&bps) {
        return Err(BorrowError::IncentiveOutOfBounds);
    }
    Ok(())
}

fn require_admin(env: &Env, admin: &Address) -> Result<(), BorrowError> {
    let current = get_admin(env).ok_or(BorrowError::Unauthorized)?;
    if *admin != current {
        return Err(BorrowError::Unauthorized);
    }
    admin.require_auth();
    Ok(())
}

/// Set (`Some`) or clear (`None`) the liquidation incentive for one
/// collateral asset (admin only). Cleared assets fall back to the global
/// incentive.
pub fn set_asset_liquidation_incentive_bps(
    env: &Env,
    admin: &Address,
    collateral_asset: &Address,
    bps: Option<i128>,
) -> Result<(), BorrowError> {
    require_admin(env, admin)?;
    let previous = get_asset_liquidation_incentive_bps(env, collateral_asset);
    let key = BorrowDataKey::AssetLiquidationIncentiveBps(collateral_asset.clone());
    match bps {
        Some(bps) => {
            validate_liquidation_incentive(bps)?;
            env.storage().persistent().set(&key, &bps);
        }
        None => env.storage().persistent().remove(&key),
    }

    crate::events::LiqIncentiveUpdatedEvent {
        admin: admin.clone(),
        asset: Some(collateral_asset.clone()),
        previous_bps: previous,
        new_bps: get_asset_liquidation_incentive_bps(env, collateral_asset),
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);
    Ok(())
}

/// Set oracle address for price feeds (admin only). Caller must be admin and authorize.
pub fn set_oracle(env: &Env, admin: &Address, oracle: Address) -> Result<(), BorrowError> {
    let current = get_admin(env).ok_or(BorrowError::Unauthorized)?;
    if *admin != current {
        return Err(BorrowError::Unauthorized);
    }
    admin.require_auth();
    env.storage()
        .persistent()
        .set(&BorrowDataKey::OracleAddress, &oracle);
    Ok(())
}

/// Set liquidation threshold in basis points (admin only). E.g. 8000 = 80%.
pub fn set_liquidation_threshold_bps(
    env: &Env,
    admin: &Address,
    bps: i128,
) -> Result<(), BorrowError> {
    let current = get_admin(env).ok_or(BorrowError::Unauthorized)?;
    if *admin != current {
        return Err(BorrowError::Unauthorized);
    }
    admin.require_auth();
    if bps <= 0 || bps > 10000 {
        return Err(BorrowError::InvalidAmount);
    }
    env.storage()
        .persistent()
        .set(&BorrowDataKey::LiquidationThresholdBps, &bps);
    Ok(())
}

/// Set close factor in basis points (admin only). E.g. 5000 = 50%.
pub fn set_close_factor_bps(env: &Env, admin: &Address, bps: i128) -> Result<(), BorrowError> {
    let current = get_admin(env).ok_or(BorrowError::Unauthorized)?;
    if *admin != current {
        return Err(BorrowError::Unauthorized);
    }
    admin.require_auth();
    if bps <= 0 || bps > 10000 {
        return Err(BorrowError::InvalidAmount);
    }
    env.storage()
        .persistent()
        .set(&BorrowDataKey::CloseFactorBps, &bps);
    Ok(())
}

/// Set the global liquidation incentive in basis points (admin only), used
/// for assets without an override. E.g. 1000 = 10%.
pub fn set_liquidation_incentive_bps(
    env: &Env,
    admin: &Address,
    bps: i128,
) -> Result<(), BorrowError> {
    require_admin(env, admin)?;
    validate_liquidation_incentive(bps)?;
    let previous = get_liquidation_incentive_bps(env);
    env.storage()
        .persistent()
        .set(&BorrowDataKey::LiquidationIncentiveBps, &bps);

    crate::events::LiqIncentiveUpdatedEvent {
        admin: admin.clone(),
        asset: None,
        previous_bps: previous,
        new_bps: bps,
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);
    Ok(())
}

/// Get current interest index (for invariant testing)
pub fn get_interest_index(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::InterestIndex)
        .unwrap_or(1_000_000_000) // Default to 1.0 with 9 decimals
}

/// Set interest index (internal use)
pub fn set_interest_index(env: &Env, index: i128) {
    env.storage()
        .persistent()
        .set(&BorrowDataKey::InterestIndex, &index);
}

pub fn set_stablecoin_config(
    env: &Env,
    admin: &Address,
    asset: Address,
    config: StablecoinConfig,
) -> Result<(), BorrowError> {
    let current = get_admin(env).ok_or(BorrowError::Unauthorized)?;
    if *admin != current {
        return Err(BorrowError::Unauthorized);
    }
    admin.require_auth();
    env.storage()
        .persistent()
        .set(&BorrowDataKey::AssetStablecoinConfig(asset), &config);
    Ok(())
}

pub fn get_stablecoin_config(env: &Env, asset: &Address) -> Option<StablecoinConfig> {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::AssetStablecoinConfig(asset.clone()))
}

/// Query the risk pool an asset belongs to (0 = default shared pool).
pub fn get_asset_pool(env: &Env, asset: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::AssetPool(asset.clone()))
        .unwrap_or(0)
}

/// Set the risk pool an asset belongs to (admin only).
pub fn set_asset_pool(
    env: &Env,
    admin: &Address,
    asset: &Address,
    pool_id: u32,
) -> Result<(), BorrowError> {
    require_admin(env, admin)?;
    let previous = get_asset_pool(env, asset);
    env.storage()
        .persistent()
        .set(&BorrowDataKey::AssetPool(asset.clone()), &pool_id);

    crate::events::AssetPoolUpdatedEvent {
        admin: admin.clone(),
        asset: asset.clone(),
        previous_pool_id: previous,
        new_pool_id: pool_id,
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);
    Ok(())
}

/// Query the debt ceiling for an isolated risk pool (0 = uncapped).
pub fn get_pool_debt_ceiling(env: &Env, pool_id: u32) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::PoolDebtCeiling(pool_id))
        .unwrap_or(0)
}

/// Set the aggregate debt ceiling for an isolated risk pool (admin only).
pub fn set_pool_debt_ceiling(
    env: &Env,
    admin: &Address,
    pool_id: u32,
    ceiling: i128,
) -> Result<(), BorrowError> {
    require_admin(env, admin)?;
    if ceiling < 0 {
        return Err(BorrowError::InvalidAmount);
    }
    let previous = get_pool_debt_ceiling(env, pool_id);
    env.storage()
        .persistent()
        .set(&BorrowDataKey::PoolDebtCeiling(pool_id), &ceiling);

    crate::events::PoolDebtCeilingUpdatedEvent {
        admin: admin.clone(),
        pool_id,
        previous_ceiling: previous,
        new_ceiling: ceiling,
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);
    Ok(())
}

/// Query current total active debt in an isolated pool.
pub fn get_pool_total_debt(env: &Env, pool_id: u32) -> i128 {
    env.storage()
        .persistent()
        .get(&BorrowDataKey::PoolTotalDebt(pool_id))
        .unwrap_or(0)
}

pub(crate) fn set_pool_total_debt(env: &Env, pool_id: u32, total: i128) {
    env.storage()
        .persistent()
        .set(&BorrowDataKey::PoolTotalDebt(pool_id), &total);
}

pub(crate) fn decrement_pool_total_debt(env: &Env, pool_id: u32, amount: i128) {
    if pool_id > 0 && amount > 0 {
        let current = get_pool_total_debt(env, pool_id);
        let updated = current.saturating_sub(amount);
        set_pool_total_debt(env, pool_id, updated);
    }
}

// Unified error registry: protocol-wide global codes, messages and recovery
// suggestions for every variant (see `stellarlend_errors` and docs/ERROR_HANDLING.md).
stellarlend_errors::impl_contract_error! {
    BorrowError => stellarlend_errors::domains::LENDING_BORROW;
    InsufficientCollateral => Insufficient, "Collateral amount does not meet the 150% minimum ratio", AddCollateral;
    DebtCeilingReached => LimitExceeded, "Total protocol debt would exceed the configured debt ceiling", ReduceAmount;
    ProtocolPaused => Paused, "Borrow operations are currently paused";
    InvalidAmount => InvalidInput, "Borrow or collateral amount is zero or negative";
    Overflow => Overflow, "Arithmetic overflow during calculation";
    Unauthorized => Unauthorized, "Caller is not authorized for this operation";
    AssetNotSupported => InvalidAsset, "The requested asset is not supported for borrowing";
    BelowMinimumBorrow => InvalidInput, "Borrow amount is below the configured minimum";
    RepayAmountTooHigh => InvalidInput, "Repay amount exceeds current debt", ReduceAmount;
    PositionHealthy => InvalidState, "Position is healthy and cannot be liquidated";
    InsufficientReserves => Insufficient, "Insufficient reserves to recover bad debt", ContactAdmin;
    ReentrancyDetected => Reentrancy, "Reentrant call detected";
    UtilizationThrottled => LimitExceeded, "Utilization circuit breaker is throttling new borrows", RetryLater;
    TooManyCollateralAssets => LimitExceeded, "Position already holds the maximum number of collateral assets";
    SelfLiquidation => Unauthorized, "A borrower cannot liquidate their own position";
    PriceUnavailable => InvalidState, "The oracle has no usable price for a required asset", RetryLater;
    IncentiveOutOfBounds => InvalidInput, "Liquidation incentive is outside the allowed bounds";
    IsolatedPoolMismatch => GuaranteeViolated, "Operation violates isolated risk pool boundaries", UseSupportedAsset;
}
