//! # Liquidation of multi-collateral borrow positions
//!
//! A position becomes liquidatable once its health factor, computed over
//! **every** collateral asset it holds, falls below 1.0. A liquidator then
//! repays part of the borrower's debt in `debt_asset` and seizes one chosen
//! `collateral_asset`, worth the repaid value plus that asset's liquidation
//! incentive.
//!
//! ## Rules
//!
//! - Repayment is capped by the close factor (`total_debt * close_factor`).
//! - Seized collateral is `repay * debt_price * (1 + incentive) / collateral_price`,
//!   using the per-asset incentive of the seized asset
//!   ([`get_asset_liquidation_incentive_bps`]).
//! - If the chosen asset's balance cannot cover that, the whole balance is
//!   seized and the repayment shrinks to match, so a liquidator always gets
//!   the incentive they were promised.
//! - Every collateral asset must have a usable oracle price. A missing price
//!   aborts the liquidation instead of counting that collateral as worthless,
//!   so an oracle outage cannot make a healthy position liquidatable.
//! - Rounding favours the borrower: seized amounts round down.
//!
//! ## Settlement
//!
//! Liquidation settles on the same ledger as the rest of the borrow module:
//! like `repay`, it reduces the borrower's recorded debt (interest first,
//! then principal), and it moves the seized collateral from the borrower's
//! position to the liquidator's. No tokens move, because borrow positions
//! hold no tokens in custody.

use soroban_sdk::{contracttype, Address, Env, Map, I256};

use crate::borrow::{
    add_collateral, calculate_interest, debit_collateral_balances, decrease_asset_total_debt,
    get_asset_liquidation_incentive_bps, get_close_factor_bps, get_collateral_balances,
    get_debt_ceiling, get_debt_position, get_oracle, get_total_debt, save_debt_position,
    set_total_debt, BorrowError, RateType,
};
use crate::events::LiquidationEvent;
use crate::reentrancy::ReentrancyGuard;
use crate::views::{compute_health_factor, debt_value, HEALTH_FACTOR_SCALE};

/// Oracle prices carry 8 decimals.
const PRICE_SCALE: i128 = 100_000_000;
const BPS_SCALE: i128 = 10_000;

/// What a liquidation did, in raw asset units.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LiquidationResult {
    /// Debt repaid, in `debt_asset` units.
    pub debt_repaid: i128,
    /// Collateral moved to the liquidator, in `collateral_asset` units.
    pub collateral_seized: i128,
    /// Incentive applied to the seized asset, in basis points.
    pub incentive_bps: i128,
}

fn price(env: &Env, oracle: &Address, asset: &Address) -> Result<i128, BorrowError> {
    let price = crate::borrow::get_asset_price(env, oracle, asset);
    if price <= 0 {
        return Err(BorrowError::PriceUnavailable);
    }
    Ok(price)
}

/// `a * b / c` through I256, so products of amounts and prices cannot overflow.
fn mul_div(env: &Env, a: i128, b: i128, c: i128) -> Result<i128, BorrowError> {
    if c == 0 {
        return Err(BorrowError::Overflow);
    }
    I256::from_i128(env, a)
        .mul(&I256::from_i128(env, b))
        .div(&I256::from_i128(env, c))
        .to_i128()
        .ok_or(BorrowError::Overflow)
}

/// Value of every asset in `balances`. Fails on any asset the oracle cannot
/// price.
fn strict_collateral_value(
    env: &Env,
    oracle: &Address,
    balances: &Map<Address, i128>,
) -> Result<i128, BorrowError> {
    let mut total: i128 = 0;
    for (asset, amount) in balances.iter() {
        let value = mul_div(env, amount, price(env, oracle, &asset)?, PRICE_SCALE)?;
        total = total.checked_add(value).ok_or(BorrowError::Overflow)?;
    }
    Ok(total)
}

/// Liquidate up to `amount` of `borrower`'s `debt_asset` debt, seizing
/// `collateral_asset`. The caller has already authorized `liquidator`.
pub fn liquidate(
    env: &Env,
    liquidator: Address,
    borrower: Address,
    debt_asset: Address,
    collateral_asset: Address,
    amount: i128,
) -> Result<LiquidationResult, BorrowError> {
    let _guard = ReentrancyGuard::new(env).map_err(|_| BorrowError::ReentrancyDetected)?;

    if amount <= 0 {
        return Err(BorrowError::InvalidAmount);
    }
    if liquidator == borrower {
        return Err(BorrowError::SelfLiquidation);
    }
    let oracle = get_oracle(env).ok_or(BorrowError::PriceUnavailable)?;

    // Debt in the requested asset: variable bucket first, then stable, as `repay` does.
    // The variable record is read once and reused when it is the selected bucket.
    let variable = get_debt_position(env, &borrower, Some(&debt_asset), RateType::Variable);
    let mut debt = if variable.borrowed_amount > 0 || variable.interest_accrued > 0 {
        variable
    } else {
        get_debt_position(env, &borrower, Some(&debt_asset), RateType::Stable)
    };
    if debt.borrowed_amount == 0 && debt.interest_accrued == 0 {
        return Err(BorrowError::PositionHealthy);
    }
    if debt.asset != debt_asset {
        return Err(BorrowError::AssetNotSupported);
    }

    // One read of the collateral record serves the health check, the
    // availability check and the seizure below.
    let mut balances = get_collateral_balances(env, &borrower);

    // Interest on the selected position, computed once. An error is only
    // reported after the health and availability checks, as it always was.
    let accrued = calculate_interest(env, &debt);

    // Health factor over the whole position, matching `get_health_factor`. The
    // debt side is the view `get_user_debt` reports: it saturates on overflow
    // and ignores any other interest error.
    let collateral_value = strict_collateral_value(env, &oracle, &balances)?;
    let mut debt_view = debt.clone();
    match accrued {
        Ok(interest) => {
            debt_view.interest_accrued = debt_view.interest_accrued.saturating_add(interest);
        }
        Err(BorrowError::Overflow) => debt_view.interest_accrued = i128::MAX,
        Err(_) => {}
    }
    let position_debt_value = debt_value(env, &debt_view);
    if position_debt_value <= 0 {
        return Err(BorrowError::PriceUnavailable);
    }
    let health_factor = compute_health_factor(env, collateral_value, position_debt_value, true);
    if health_factor >= HEALTH_FACTOR_SCALE {
        return Err(BorrowError::PositionHealthy);
    }

    let available = balances.get(collateral_asset.clone()).unwrap_or(0);
    if available <= 0 {
        return Err(BorrowError::InsufficientCollateral);
    }

    // Bring interest up to date before sizing the repayment.
    let accrued = accrued?;
    debt.interest_accrued = debt
        .interest_accrued
        .checked_add(accrued)
        .ok_or(BorrowError::Overflow)?;
    debt.last_update = env.ledger().timestamp();
    let total_debt = debt
        .borrowed_amount
        .checked_add(debt.interest_accrued)
        .ok_or(BorrowError::Overflow)?;

    let max_repay = mul_div(env, total_debt, get_close_factor_bps(env), BPS_SCALE)?;
    let mut repay = amount.min(max_repay);

    let incentive_bps = get_asset_liquidation_incentive_bps(env, &collateral_asset);
    let debt_price = price(env, &oracle, &debt_asset)?;
    let collateral_price = price(env, &oracle, &collateral_asset)?;
    // Collateral units per debt unit, including the incentive:
    // debt_price * (1 + incentive) / collateral_price.
    let seize_numerator = debt_price
        .checked_mul(BPS_SCALE + incentive_bps)
        .ok_or(BorrowError::Overflow)?;
    let seize_denominator = collateral_price
        .checked_mul(BPS_SCALE)
        .ok_or(BorrowError::Overflow)?;

    let mut seize = mul_div(env, repay, seize_numerator, seize_denominator)?;
    if seize > available {
        // Not enough of this asset: take all of it and repay only what it covers.
        repay = mul_div(env, available, seize_denominator, seize_numerator)?;
        seize = mul_div(env, repay, seize_numerator, seize_denominator)?;
    }
    if repay <= 0 || seize <= 0 {
        return Err(BorrowError::InvalidAmount);
    }

    // Interest first, then principal (the same order as `repay`).
    let interest_paid = repay.min(debt.interest_accrued);
    debt.interest_accrued -= interest_paid;
    let principal_paid = repay - interest_paid;
    debt.borrowed_amount -= principal_paid;
    save_debt_position(env, &borrower, &debt);

    if principal_paid > 0 {
        let new_total = get_total_debt(env)
            .checked_sub(principal_paid)
            .ok_or(BorrowError::Overflow)?;
        set_total_debt(env, new_total);
        decrease_asset_total_debt(env, &debt_asset, principal_paid);
        crate::utilization_breaker::on_debt_changed(env, new_total, get_debt_ceiling(env));
    }

    debit_collateral_balances(env, &borrower, &mut balances, &collateral_asset, seize)?;
    add_collateral(env, &liquidator, &collateral_asset, seize)?;

    // The part of the seizure above the repaid value, in collateral units.
    let incentive_amount = mul_div(env, seize, incentive_bps, BPS_SCALE + incentive_bps)?;

    LiquidationEvent {
        liquidator,
        borrower,
        debt_asset: Some(debt_asset),
        collateral_asset: Some(collateral_asset),
        debt_liquidated: repay,
        collateral_seized: seize,
        incentive_amount,
        timestamp: env.ledger().timestamp(),
    }
    .publish(env);

    Ok(LiquidationResult {
        debt_repaid: repay,
        collateral_seized: seize,
        incentive_bps,
    })
}
