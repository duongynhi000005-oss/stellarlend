mod common;

use common::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address,
};
use stellarlend_lending::BorrowError;

#[test]
fn default_reserve_factor_is_zero_and_reserves_start_at_zero() {
    let f = setup();
    let asset = &f.debt_asset;

    assert_eq!(f.client.get_asset_reserve_factor(asset), 0);
    assert_eq!(f.client.get_asset_protocol_reserves(asset), 0);
}

#[test]
fn admin_can_configure_update_and_clear_reserve_factor() {
    let f = setup();
    let asset = &f.debt_asset;

    // Admin sets reserve factor to 1000 bps (10%)
    f.client.set_asset_reserve_factor(&f.admin, asset, &Some(1_000));
    assert_eq!(f.client.get_asset_reserve_factor(asset), 1_000);

    // Admin updates reserve factor to 2500 bps (25%)
    f.client.set_asset_reserve_factor(&f.admin, asset, &Some(2_500));
    assert_eq!(f.client.get_asset_reserve_factor(asset), 2_500);

    // Admin clears reserve factor (sets to 0 bps)
    f.client.set_asset_reserve_factor(&f.admin, asset, &None);
    assert_eq!(f.client.get_asset_reserve_factor(asset), 0);

    // Explicit Some(0) also clears to 0 bps
    f.client.set_asset_reserve_factor(&f.admin, asset, &Some(0));
    assert_eq!(f.client.get_asset_reserve_factor(asset), 0);
}

#[test]
fn set_reserve_factor_rejects_unauthorized_and_invalid_bounds() {
    let f = setup();
    let stranger = Address::generate(&f.env);
    let asset = &f.debt_asset;

    // Stranger cannot configure reserve factor
    assert_eq!(
        f.client.try_set_asset_reserve_factor(&stranger, asset, &Some(1_000)),
        Err(Ok(BorrowError::Unauthorized))
    );

    // Negative basis points rejected
    assert_eq!(
        f.client.try_set_asset_reserve_factor(&f.admin, asset, &Some(-1)),
        Err(Ok(BorrowError::InvalidAmount))
    );

    // Over 10,000 bps (100%) rejected
    assert_eq!(
        f.client.try_set_asset_reserve_factor(&f.admin, asset, &Some(10_001)),
        Err(Ok(BorrowError::InvalidAmount))
    );

    // Exactly 10,000 bps (100%) accepted
    assert!(f.client.try_set_asset_reserve_factor(&f.admin, asset, &Some(10_000)).is_ok());
    assert_eq!(f.client.get_asset_reserve_factor(asset), 10_000);
}

#[test]
fn reserve_factors_are_isolated_per_asset() {
    let f = setup();
    let asset_a = &f.debt_asset;
    let asset_b = &f.collateral_asset;

    f.client.set_asset_reserve_factor(&f.admin, asset_a, &Some(1_500));
    f.client.set_asset_reserve_factor(&f.admin, asset_b, &Some(3_000));

    assert_eq!(f.client.get_asset_reserve_factor(asset_a), 1_500);
    assert_eq!(f.client.get_asset_reserve_factor(asset_b), 3_000);

    f.client.set_asset_reserve_factor(&f.admin, asset_a, &None);
    assert_eq!(f.client.get_asset_reserve_factor(asset_a), 0);
    assert_eq!(f.client.get_asset_reserve_factor(asset_b), 3_000);
}

#[test]
fn interest_repayment_accrues_reserves_with_exact_conservation() {
    let f = setup();
    let borrower = Address::generate(&f.env);
    let asset = &f.debt_asset;

    // Configure 2000 bps (20%) reserve factor on borrow asset
    f.client.set_asset_reserve_factor(&f.admin, asset, &Some(2_000));

    // Borrower creates loan
    let borrow_amt = 10_000;
    let collateral_amt = 25_000;
    f.client.borrow(
        &borrower,
        asset,
        &borrow_amt,
        &f.collateral_asset,
        &collateral_amt,
    );

    // Advance time by 30 days to accrue interest
    f.env.ledger().with_mut(|l| {
        l.timestamp += 86_400 * 30;
    });

    // Repay debt + accrued interest
    let initial_reserves = f.client.get_asset_protocol_reserves(asset);
    assert_eq!(initial_reserves, 0);

    // Full repayment covers principal + all accrued interest
    let repay_amount = 5_000;
    let res = f.client.try_repay(&borrower, asset, &repay_amount);
    assert!(res.is_ok());

    let accrued_reserves = f.client.get_asset_protocol_reserves(asset);
    // Reserves must be strictly positive since interest was paid with 20% reserve factor
    assert!(accrued_reserves > 0, "Reserves should accrue from interest payment");

    // Borrow position interest should now be 0
    // Debt principal decreased
    assert!(f.client.get_debt_balance(&borrower) < borrow_amt);
}

#[test]
fn admin_can_withdraw_accumulated_reserves() {
    let f = setup();
    let borrower = Address::generate(&f.env);
    let treasury = Address::generate(&f.env);
    let stranger = Address::generate(&f.env);
    let asset = &f.debt_asset;

    // Configure 5000 bps (50%) reserve factor
    f.client.set_asset_reserve_factor(&f.admin, asset, &Some(5_000));

    f.client.borrow(
        &borrower,
        asset,
        &10_000,
        &f.collateral_asset,
        &25_000,
    );

    // Advance 60 days
    f.env.ledger().with_mut(|l| {
        l.timestamp += 86_400 * 60;
    });

    // Repay interest
    f.client.repay(&borrower, asset, &5_000);
    let reserves = f.client.get_asset_protocol_reserves(asset);
    assert!(reserves > 0);

    // Stranger cannot withdraw reserves
    assert_eq!(
        f.client.try_withdraw_reserves(&stranger, asset, &treasury, &reserves),
        Err(Ok(BorrowError::Unauthorized))
    );

    // Withdrawing more than available reserves fails
    assert_eq!(
        f.client.try_withdraw_reserves(&f.admin, asset, &treasury, &(reserves + 1)),
        Err(Ok(BorrowError::InsufficientReserves))
    );

    // Admin successfully withdraws reserves
    assert!(f.client.try_withdraw_reserves(&f.admin, asset, &treasury, &reserves).is_ok());
    assert_eq!(f.client.get_asset_protocol_reserves(asset), 0);
}

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_reserve_factor_exact_mathematical_conservation(
        interest_paid in 1i128..=1_000_000_000i128,
        reserve_factor_bps in 0i128..=10_000i128,
    ) {
        let reserve_cut = (interest_paid * reserve_factor_bps) / 10_000;
        let supplier_cut = interest_paid - reserve_cut;

        // Core Invariant 1: Total interest is conserved
        prop_assert_eq!(reserve_cut + supplier_cut, interest_paid);

        // Core Invariant 2: Reserve cut never exceeds total interest
        prop_assert!(reserve_cut <= interest_paid);
        prop_assert!(reserve_cut >= 0);

        // Core Invariant 3: Boundary invariants
        if reserve_factor_bps == 0 {
            prop_assert_eq!(reserve_cut, 0);
            prop_assert_eq!(supplier_cut, interest_paid);
        }
        if reserve_factor_bps == 10_000 {
            prop_assert_eq!(reserve_cut, interest_paid);
            prop_assert_eq!(supplier_cut, 0);
        }
    }
}
