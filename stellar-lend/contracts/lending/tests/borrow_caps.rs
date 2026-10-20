mod common;

use common::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger as _},
    Address,
};
use stellarlend_lending::{BorrowError, RateType};
use test_utils::snapshotless_env;

#[test]
fn default_asset_borrow_is_uncapped_and_tracks_total_debt() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let (ca, da) = (&f.collateral_asset, &f.debt_asset);

    assert_eq!(f.client.get_asset_borrow_cap(da), None);
    assert_eq!(f.client.get_asset_total_debt(da), 0);

    f.client.borrow(&alice, da, &3_000, ca, &4_500);
    assert_eq!(f.client.get_asset_total_debt(da), 3_000);

    f.client.borrow(&bob, da, &2_000, ca, &3_000);
    assert_eq!(f.client.get_asset_total_debt(da), 5_000);
}

#[test]
fn admin_can_configure_update_and_clear_per_asset_borrow_cap() {
    let f = setup();
    let user = Address::generate(&f.env);
    let (ca, da) = (&f.collateral_asset, &f.debt_asset);

    f.client.set_asset_borrow_cap(&f.admin, da, &Some(4_000));
    assert!(!f.env.events().all().events().is_empty());
    assert_eq!(f.client.get_asset_borrow_cap(da), Some(4_000));

    // Borrow above 4_000 is rejected while capped.
    assert_eq!(
        f.client.try_borrow(&user, da, &5_000, ca, &7_500),
        Err(Ok(BorrowError::DebtCeilingReached))
    );

    // Raising the cap allows the borrow.
    f.client.set_asset_borrow_cap(&f.admin, da, &Some(6_000));
    assert_eq!(f.client.get_asset_borrow_cap(da), Some(6_000));
    f.client.borrow(&user, da, &5_000, ca, &7_500);
    assert_eq!(f.client.get_asset_total_debt(da), 5_000);

    // Clearing the cap removes the per-asset limit.
    f.client.set_asset_borrow_cap(&f.admin, da, &None);
    assert_eq!(f.client.get_asset_borrow_cap(da), None);
    f.client.borrow(&user, da, &3_000, ca, &4_500);
    assert_eq!(f.client.get_asset_total_debt(da), 8_000);
}

#[test]
fn set_asset_borrow_cap_rejects_unauthorized_and_non_positive_values() {
    let f = setup();
    let attacker = Address::generate(&f.env);
    let da = &f.debt_asset;

    assert_eq!(
        f.client
            .try_set_asset_borrow_cap(&attacker, da, &Some(10_000)),
        Err(Ok(BorrowError::Unauthorized))
    );
    assert_eq!(
        f.client.try_set_asset_borrow_cap(&f.admin, da, &Some(0)),
        Err(Ok(BorrowError::InvalidAmount))
    );
    assert_eq!(
        f.client.try_set_asset_borrow_cap(&f.admin, da, &Some(-250)),
        Err(Ok(BorrowError::InvalidAmount))
    );
    assert_eq!(f.client.get_asset_borrow_cap(da), None);

    // Also verify real signature check without mocked auths.
    let env = snapshotless_env();
    env.mock_all_auths();
    let (client, admin) = deploy(&env);
    env.set_auths(&[]);
    let unauthed_asset = Address::generate(&env);
    assert!(client
        .try_set_asset_borrow_cap(&admin, &unauthed_asset, &Some(10_000))
        .is_err());
}

#[test]
fn variable_and_stable_borrows_enforce_per_asset_borrow_cap() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let (ca, da) = (&f.collateral_asset, &f.debt_asset);

    f.client.set_asset_borrow_cap(&f.admin, da, &Some(10_000));

    f.client
        .borrow_with_rate(&alice, da, &6_000, ca, &9_000, &RateType::Variable);
    assert_eq!(f.client.get_asset_total_debt(da), 6_000);

    // Exact cap boundary succeeds across users and rate types.
    f.client
        .borrow_with_rate(&bob, da, &4_000, ca, &6_000, &RateType::Stable);
    assert_eq!(f.client.get_asset_total_debt(da), 10_000);

    // Any additional borrow for this asset is rejected and state is preserved.
    assert_eq!(
        f.client.try_borrow(&bob, da, &100, ca, &150),
        Err(Ok(BorrowError::DebtCeilingReached))
    );
    assert_eq!(f.client.get_asset_total_debt(da), 10_000);
    assert_eq!(f.client.get_user_debt(&bob).borrowed_amount, 4_000);
}

#[test]
fn borrow_caps_are_isolated_per_asset() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let ca = &f.collateral_asset;
    let asset_a = Address::generate(&f.env);
    let asset_b = Address::generate(&f.env);
    let asset_c = Address::generate(&f.env);

    f.client
        .set_asset_borrow_cap(&f.admin, &asset_a, &Some(5_000));
    f.client
        .set_asset_borrow_cap(&f.admin, &asset_b, &Some(12_000));

    f.client.borrow(&alice, &asset_a, &5_000, ca, &7_500);
    assert_eq!(
        f.client.try_borrow(&bob, &asset_a, &100, ca, &150),
        Err(Ok(BorrowError::DebtCeilingReached))
    );

    // Asset B and uncapped Asset C remain unaffected by Asset A reaching its borrow cap.
    f.client.borrow(&bob, &asset_b, &12_000, ca, &18_000);
    assert_eq!(
        f.client.try_borrow(&bob, &asset_b, &100, ca, &150),
        Err(Ok(BorrowError::DebtCeilingReached))
    );

    let charlie = Address::generate(&f.env);
    f.client.borrow(&charlie, &asset_c, &25_000, ca, &37_500);
    assert_eq!(f.client.get_asset_total_debt(&asset_a), 5_000);
    assert_eq!(f.client.get_asset_total_debt(&asset_b), 12_000);
    assert_eq!(f.client.get_asset_total_debt(&asset_c), 25_000);
}

#[test]
fn repayments_and_liquidations_free_per_asset_borrow_capacity() {
    let f = setup();
    let borrower = Address::generate(&f.env);
    let second_borrower = Address::generate(&f.env);
    let liquidator = Address::generate(&f.env);
    let (ca, da) = (&f.collateral_asset, &f.debt_asset);

    f.client.set_asset_borrow_cap(&f.admin, da, &Some(10_000));
    f.client.borrow(&borrower, da, &10_000, ca, &15_000);
    assert_eq!(
        f.client
            .try_borrow(&second_borrower, da, &1_000, ca, &1_500),
        Err(Ok(BorrowError::DebtCeilingReached))
    );

    // Partial repayment frees capacity under the asset cap.
    f.client.repay(&borrower, da, &4_000);
    assert_eq!(f.client.get_asset_total_debt(da), 6_000);

    f.client.borrow(&second_borrower, da, &4_000, ca, &6_000);
    assert_eq!(f.client.get_asset_total_debt(da), 10_000);

    // Price drop makes `borrower` liquidatable; liquidation reduces per-asset debt.
    f.oracle_client().set_price(ca, &(PRICE_SCALE / 3));
    f.env.ledger().with_mut(|l| l.timestamp += 1);
    let res = f.client.liquidate(&liquidator, &borrower, da, ca, &2_000);
    assert!(res.debt_repaid > 0);
    assert!(f.client.get_asset_total_debt(da) < 10_000);

    // Restore oracle price and verify freed borrow capacity can be borrowed again.
    f.oracle_client().set_price(ca, &PRICE_SCALE);
    let freed = 10_000 - f.client.get_asset_total_debt(da);
    assert!(freed >= MIN_BORROW);
    f.client
        .borrow(&second_borrower, da, &freed, ca, &(freed * 2));
    assert_eq!(f.client.get_asset_total_debt(da), 10_000);
}
