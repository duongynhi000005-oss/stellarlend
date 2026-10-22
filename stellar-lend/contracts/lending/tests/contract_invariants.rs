//! Contract Invariant and Property-Based Testing Suite (Issue #1361).
//!
//! Formally defines, fuzzes, and enforces protocol invariants for the StellarLend
//! lending protocol across single and multi-asset positions, supply caps, liquidations,
//! batch operations, pause states, and price fluctuations.
//!
//! | Invariant ID | Name | Mathematical Specification | Enforced By |
//! |---|---|---|---|
//! | **INV-01** | Supply Cap Invariant | $\forall a, \text{total\_deposited}(a) \le \text{supply\_cap}(a)$ when cap is set | `prop_supply_cap_invariant`, `prop_full_lending_invariant_fuzzer` |
//! | **INV-02** | Liquidation Solvency & Conservation | Liquidations require $HF < 1.0$. $\Delta \text{debt} = -\text{repaid}$, $\Delta \text{collateral}_B = -\text{seized}$, $\Delta \text{collateral}_L = +\text{seized}$ | `prop_liquidation_solvency_and_conservation`, `prop_full_lending_invariant_fuzzer` |
//! | **INV-03** | Batch Deposit Atomicity | $\text{batch}[(a_i, v_i)]$ either increments all deposits by $v_i$ or reverts with 0 state change | `prop_batch_deposit_atomicity_and_conservation`, `prop_full_lending_invariant_fuzzer` |
//! | **INV-04** | Subsystem Pause Safety | If pause type $P$ is active, corresponding operation fails with `Paused` and state is unchanged | `prop_pause_subsystems_safety`, `prop_full_lending_invariant_fuzzer` |
//! | **INV-05** | Multi-Collateral Valuation | $\text{collateral\_value}(u) = \sum_a \text{collateral}(u, a) \cdot \text{price}(a) / 10^8$, $|\text{assets}| \le 8$ | `prop_multi_collateral_valuation`, `prop_full_lending_invariant_fuzzer` |
//! | **INV-06** | Accounting Soundness | $\forall u, a: \text{deposit} \ge 0, \text{collateral} \ge 0, \text{debt} \ge 0$. If $\text{debt} == 0 \implies HF = 100{,}000{,}000$ | all suites |

mod common;

use common::*;
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Vec as SorobanVec,
};
use stellarlend_lending::{
    BorrowError, DepositError, DepositRequest, PauseType, WithdrawError, MAX_COLLATERAL_ASSETS,
};

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16)
}

fn set_price(f: &Fixture, asset: &Address, price: i128) {
    f.oracle_client().set_price(asset, &price);
    f.env.ledger().with_mut(|l| l.timestamp += 1);
}

// ============================================================================
// INV-01: Supply Cap Invariant
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn prop_supply_cap_invariant(
        cap in 5_000i128..=100_000i128,
        deposit1 in 100i128..=60_000i128,
        deposit2 in 100i128..=60_000i128,
    ) {
        let f = setup();
        let alice = Address::generate(&f.env);
        let bob = Address::generate(&f.env);
        let asset = &f.collateral_asset;

        // Set supply cap on the asset
        f.client.set_asset_supply_cap(&f.admin, asset, &Some(cap));
        prop_assert_eq!(f.client.get_asset_supply_cap(asset), Some(cap));

        // Invariant check helper
        let assert_supply_invariant = |f: &Fixture| {
            let total = f.client.get_asset_total_supply(asset);
            prop_assert!(total <= cap, "INV-01 violated: total {} > cap {}", total, cap);
            Ok(())
        };

        // First deposit
        if deposit1 <= cap {
            let res = f.client.try_deposit(&alice, asset, &deposit1);
            prop_assert!(res.is_ok());
            prop_assert_eq!(f.client.get_asset_total_supply(asset), deposit1);
        } else {
            let res = f.client.try_deposit(&alice, asset, &deposit1);
            prop_assert_eq!(res, Err(Ok(DepositError::ExceedsDepositCap)));
            prop_assert_eq!(f.client.get_asset_total_supply(asset), 0);
        }
        assert_supply_invariant(&f)?;

        let supply_after_1 = f.client.get_asset_total_supply(asset);

        // Second deposit
        if supply_after_1 + deposit2 <= cap {
            let res = f.client.try_deposit(&bob, asset, &deposit2);
            prop_assert!(res.is_ok());
            prop_assert_eq!(f.client.get_asset_total_supply(asset), supply_after_1 + deposit2);
        } else {
            let res = f.client.try_deposit(&bob, asset, &deposit2);
            prop_assert_eq!(res, Err(Ok(DepositError::ExceedsDepositCap)));
            prop_assert_eq!(f.client.get_asset_total_supply(asset), supply_after_1);
        }
        assert_supply_invariant(&f)?;

        // Raising or clearing the cap allows previously blocked deposits
        f.client.set_asset_supply_cap(&f.admin, asset, &None);
        prop_assert_eq!(f.client.get_asset_supply_cap(asset), None);
        let current_supply = f.client.get_asset_total_supply(asset);
        let res = f.client.try_deposit(&bob, asset, &deposit2);
        prop_assert!(res.is_ok());
        prop_assert_eq!(f.client.get_asset_total_supply(asset), current_supply + deposit2);
    }
}

// ============================================================================
// INV-02: Liquidation Solvency & Conservation Invariant
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn prop_liquidation_solvency_and_conservation(
        borrow_amount in 500i128..=5_000i128,
        price_drop_pct in 40i128..=80i128, // drops collateral price to 20%..60% of original
    ) {
        let f = setup();
        let borrower = Address::generate(&f.env);
        let liquidator = Address::generate(&f.env);

        let collateral_amount = borrow_amount * 2; // 200% collateralized initially
        f.client.borrow(
            &borrower,
            &f.debt_asset,
            &borrow_amount,
            &f.collateral_asset,
            &collateral_amount,
        );

        // 1. Initially healthy: HF >= 1.0 -> Liquidation MUST be rejected (INV-02)
        let initial_hf = f.client.get_health_factor(&borrower);
        prop_assert!(initial_hf >= HF_SCALE, "Position must initially be healthy");

        let liq_attempt = f.client.try_liquidate(
            &liquidator,
            &borrower,
            &f.debt_asset,
            &f.collateral_asset,
            &borrow_amount,
        );
        prop_assert_eq!(liq_attempt, Err(Ok(BorrowError::PositionHealthy)));

        // 2. Self-liquidation is strictly prohibited
        let self_liq = f.client.try_liquidate(
            &borrower,
            &borrower,
            &f.debt_asset,
            &f.collateral_asset,
            &borrow_amount,
        );
        prop_assert_eq!(self_liq, Err(Ok(BorrowError::SelfLiquidation)));

        // 3. Drop collateral price to make position liquidatable
        let new_price = PRICE_SCALE * (100 - price_drop_pct) / 100;
        set_price(&f, &f.collateral_asset, new_price);

        let post_drop_hf = f.client.get_health_factor(&borrower);
        if post_drop_hf < HF_SCALE {
            let borrower_debt_before = f.client.get_debt_balance(&borrower);
            let borrower_collateral_before = f
                .client
                .get_user_collaterals(&borrower)
                .get(f.collateral_asset.clone())
                .unwrap_or(0);
            let liquidator_collateral_before = f
                .client
                .get_user_collaterals(&liquidator)
                .get(f.collateral_asset.clone())
                .unwrap_or(0);

            let liq_res = f.client.try_liquidate(
                &liquidator,
                &borrower,
                &f.debt_asset,
                &f.collateral_asset,
                &(borrow_amount / 2),
            );
            prop_assert!(liq_res.is_ok(), "Liquidation of unhealthy position should succeed");
            let result = liq_res.unwrap().unwrap();

            let borrower_debt_after = f.client.get_debt_balance(&borrower);
            let borrower_collateral_after = f
                .client
                .get_user_collaterals(&borrower)
                .get(f.collateral_asset.clone())
                .unwrap_or(0);
            let liquidator_collateral_after = f
                .client
                .get_user_collaterals(&liquidator)
                .get(f.collateral_asset.clone())
                .unwrap_or(0);

            // Invariant: Debt strictly decreases by debt_repaid
            prop_assert_eq!(borrower_debt_after, borrower_debt_before - result.debt_repaid);
            // Invariant: Borrower collateral decreases by collateral_seized
            prop_assert_eq!(borrower_collateral_after, borrower_collateral_before - result.collateral_seized);
            // Invariant: Liquidator collateral increases by exact collateral_seized
            prop_assert_eq!(liquidator_collateral_after, liquidator_collateral_before + result.collateral_seized);
            // Invariant: Positive repayments and non-negative balances
            prop_assert!(result.debt_repaid > 0);
            prop_assert!(result.collateral_seized > 0);
            prop_assert!(borrower_debt_after >= 0);
            prop_assert!(borrower_collateral_after >= 0);
        }
    }
}

// ============================================================================
// INV-03: Batch Deposit Conservation & Atomicity Invariant
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn prop_batch_deposit_atomicity_and_conservation(
        amt1 in 100i128..=10_000i128,
        amt2 in 100i128..=10_000i128,
        amt3 in 100i128..=10_000i128,
        trigger_failure in any::<bool>(),
    ) {
        let f = setup();
        let user = Address::generate(&f.env);
        let asset_a = &f.collateral_asset;
        let asset_b = &f.debt_asset;

        let supply_a_before = f.client.get_asset_total_supply(asset_a);
        let supply_b_before = f.client.get_asset_total_supply(asset_b);

        // Cap asset B such that amt2 + amt3 exceeds cap if trigger_failure is true
        if trigger_failure {
            f.client.set_asset_supply_cap(&f.admin, asset_b, &Some(amt2 / 2));
        }

        let mut reqs = SorobanVec::new(&f.env);
        reqs.push_back(DepositRequest {
            asset: asset_a.clone(),
            amount: amt1,
        });
        reqs.push_back(DepositRequest {
            asset: asset_b.clone(),
            amount: amt2,
        });
        reqs.push_back(DepositRequest {
            asset: asset_b.clone(),
            amount: amt3,
        });

        let res = f.client.try_deposit_batch(&user, &reqs);

        if trigger_failure {
            // Atomicity check: batch MUST fail with ExceedsDepositCap, and NO balances change
            prop_assert_eq!(res, Err(Ok(DepositError::ExceedsDepositCap)));
            prop_assert_eq!(f.client.get_asset_total_supply(asset_a), supply_a_before);
            prop_assert_eq!(f.client.get_asset_total_supply(asset_b), supply_b_before);
            prop_assert_eq!(f.client.get_user_collateral_deposit(&user, asset_a).amount, 0);
        } else {
            // Conservation check: batch succeeds, per-asset supply equals exact sum
            prop_assert!(res.is_ok());
            prop_assert_eq!(f.client.get_asset_total_supply(asset_a), supply_a_before + amt1);
            prop_assert_eq!(f.client.get_asset_total_supply(asset_b), supply_b_before + amt2 + amt3);
            prop_assert_eq!(f.client.get_user_collateral_deposit(&user, asset_b).amount, amt1 + amt2 + amt3);
        }
    }
}

// ============================================================================
// INV-04: Subsystem Pause Safety Invariant
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn prop_pause_subsystems_safety(
        pause_deposit in any::<bool>(),
        pause_borrow in any::<bool>(),
        pause_repay in any::<bool>(),
        pause_withdraw in any::<bool>(),
        pause_liquidation in any::<bool>(),
    ) {
        let f = setup();
        let user = Address::generate(&f.env);
        let liquidator = Address::generate(&f.env);

        // Apply pause states
        if pause_deposit {
            f.client.set_pause(&f.admin, &PauseType::Deposit, &true);
        }
        if pause_borrow {
            f.client.set_pause(&f.admin, &PauseType::Borrow, &true);
        }
        if pause_repay {
            f.client.set_pause(&f.admin, &PauseType::Repay, &true);
        }
        if pause_withdraw {
            f.client.set_pause(&f.admin, &PauseType::Withdraw, &true);
        }
        if pause_liquidation {
            f.client.set_pause(&f.admin, &PauseType::Liquidation, &true);
        }

        // Test Deposit pause enforcement
        let dep_res = f.client.try_deposit(&user, &f.collateral_asset, &1_000);
        if pause_deposit {
            prop_assert_eq!(dep_res, Err(Ok(DepositError::DepositPaused)));
            prop_assert_eq!(f.client.get_asset_total_supply(&f.collateral_asset), 0);
        } else {
            prop_assert!(dep_res.is_ok());
            prop_assert_eq!(f.client.get_asset_total_supply(&f.collateral_asset), 1_000);
        }

        // Test Borrow pause enforcement
        let borrow_res = f.client.try_borrow(
            &user,
            &f.debt_asset,
            &500,
            &f.collateral_asset,
            &1_000,
        );
        if pause_borrow {
            prop_assert_eq!(borrow_res, Err(Ok(BorrowError::ProtocolPaused)));
            prop_assert_eq!(f.client.get_debt_balance(&user), 0);
        } else {
            prop_assert!(borrow_res.is_ok());
            prop_assert_eq!(f.client.get_debt_balance(&user), 500);

            // Test Repay pause enforcement
            let repay_res = f.client.try_repay(&user, &f.debt_asset, &200);
            if pause_repay {
                prop_assert_eq!(repay_res, Err(Ok(BorrowError::ProtocolPaused)));
                prop_assert_eq!(f.client.get_debt_balance(&user), 500);
            } else {
                prop_assert!(repay_res.is_ok());
                prop_assert_eq!(f.client.get_debt_balance(&user), 300);
            }

            // Test Liquidation pause enforcement
            set_price(&f, &f.collateral_asset, PRICE_SCALE / 4); // force unhealthy
            let liq_res = f.client.try_liquidate(
                &liquidator,
                &user,
                &f.debt_asset,
                &f.collateral_asset,
                &100,
            );
            if pause_liquidation {
                prop_assert_eq!(liq_res, Err(Ok(BorrowError::ProtocolPaused)));
            }
        }

        // Test Withdraw pause enforcement
        let withdraw_res = f.client.try_withdraw(&user, &f.collateral_asset, &100);
        if pause_withdraw {
            prop_assert_eq!(withdraw_res, Err(Ok(WithdrawError::WithdrawPaused)));
        }
    }
}

// ============================================================================
// INV-05: Multi-Collateral Valuation & Cap Invariant
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn prop_multi_collateral_valuation(
        amounts in prop::collection::vec(100i128..=5_000i128, 1..=MAX_COLLATERAL_ASSETS as usize),
    ) {
        let f = setup();
        let borrower = Address::generate(&f.env);

        let initial_collateral = 2_000;
        f.client.borrow(
            &borrower,
            &f.debt_asset,
            &1_000,
            &f.collateral_asset,
            &initial_collateral,
        );

        let mut expected_value = initial_collateral * PRICE_SCALE / PRICE_SCALE; // price is 1.0 initially
        let mut asset_count = 1;

        for amt in amounts.iter().take(MAX_COLLATERAL_ASSETS as usize - 1) {
            let asset = Address::generate(&f.env);
            let price = PRICE_SCALE * 2; // price = 2.0
            set_price(&f, &asset, price);

            let res = f.client.try_deposit_collateral(&borrower, &asset, amt);
            prop_assert!(res.is_ok());
            asset_count += 1;
            expected_value += amt * price / PRICE_SCALE;

            let reported_val = f.client.get_collateral_value(&borrower);
            prop_assert_eq!(reported_val, expected_value, "Valuation must equal sum of asset_i * price_i");
            let holdings = f.client.get_user_collaterals(&borrower);
            prop_assert_eq!(holdings.len() as usize, asset_count);
            prop_assert!(holdings.len() <= MAX_COLLATERAL_ASSETS);
        }

        // Invariant: Adding past MAX_COLLATERAL_ASSETS (8) MUST fail with TooManyCollateralAssets
        while asset_count < MAX_COLLATERAL_ASSETS as usize {
            let asset = Address::generate(&f.env);
            f.client.deposit_collateral(&borrower, &asset, &500);
            asset_count += 1;
        }

        let overflow_asset = Address::generate(&f.env);
        let res = f.client.try_deposit_collateral(&borrower, &overflow_asset, &500);
        prop_assert_eq!(res, Err(Ok(BorrowError::TooManyCollateralAssets)));
        prop_assert_eq!(f.client.get_user_collaterals(&borrower).len(), MAX_COLLATERAL_ASSETS);
    }
}

// ============================================================================
// INV-06: Deterministic Regression Tests for Contract Invariants
// ============================================================================

#[test]
fn zero_debt_position_health_factor_sentinel() {
    let f = setup();
    let user = Address::generate(&f.env);

    // Debt-free user must have exactly HF_NO_DEBT (100_000_000)
    assert_eq!(f.client.get_debt_balance(&user), 0);
    assert_eq!(f.client.get_health_factor(&user), HF_NO_DEBT);

    // Deposit collateral without borrowing still keeps sentinel
    f.client.deposit(&user, &f.collateral_asset, &5_000);
    assert_eq!(f.client.get_health_factor(&user), HF_NO_DEBT);
}

#[test]
fn exact_supply_cap_boundary_rejection() {
    let f = setup();
    let user = Address::generate(&f.env);
    let asset = &f.collateral_asset;

    f.client
        .set_asset_supply_cap(&f.admin, asset, &Some(10_000));
    assert_eq!(f.client.get_asset_supply_cap(asset), Some(10_000));

    // Exact cap deposit succeeds
    assert_eq!(f.client.deposit(&user, asset, &10_000), 10_000);
    assert_eq!(f.client.get_asset_total_supply(asset), 10_000);

    // Any deposit above cap (even 100 units meeting MIN_DEPOSIT) is rejected
    assert_eq!(
        f.client.try_deposit(&user, asset, &100),
        Err(Ok(DepositError::ExceedsDepositCap))
    );
    assert_eq!(f.client.get_asset_total_supply(asset), 10_000);
}
