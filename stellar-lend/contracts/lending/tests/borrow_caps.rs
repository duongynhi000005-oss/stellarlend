mod common;

use common::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger as _},
    Address,
};
use stellarlend_lending::BorrowError;
use test_utils::snapshotless_env;

#[test]
fn default_asset_borrow_is_uncapped_and_tracks_total_borrowed() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let debt_asset = &f.debt_asset;
    let col_asset = &f.collateral_asset;

    assert_eq!(f.client.get_asset_borrow_cap(debt_asset), None);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 0);

    f.client.borrow(&alice, debt_asset, &12_000, col_asset, &24_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 12_000);

    f.client.borrow(&bob, debt_asset, &8_000, col_asset, &16_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 20_000);
}

#[test]
fn admin_can_configure_update_and_clear_per_asset_borrow_cap() {
    let f = setup();
    let user = Address::generate(&f.env);
    let debt_asset = &f.debt_asset;
    let col_asset = &f.collateral_asset;

    f.client
        .set_asset_borrow_cap(&f.admin, debt_asset, &Some(15_000));
    assert!(!f.env.events().all().events().is_empty());
    assert_eq!(f.client.get_asset_borrow_cap(debt_asset), Some(15_000));

    // Borrow above 15_000 is rejected while capped.
    assert_eq!(
        f.client
            .try_borrow(&user, debt_asset, &16_000, col_asset, &32_000),
        Err(Ok(BorrowError::ExceedsBorrowCap))
    );

    // Raising the cap allows the borrow.
    f.client
        .set_asset_borrow_cap(&f.admin, debt_asset, &Some(20_000));
    assert_eq!(f.client.get_asset_borrow_cap(debt_asset), Some(20_000));
    f.client.borrow(&user, debt_asset, &16_000, col_asset, &32_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 16_000);

    // Clearing the cap removes the per-asset limit.
    f.client.set_asset_borrow_cap(&f.admin, debt_asset, &None);
    assert_eq!(f.client.get_asset_borrow_cap(debt_asset), None);
    f.client.borrow(&user, debt_asset, &10_000, col_asset, &20_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 26_000);
}

#[test]
fn set_asset_borrow_cap_rejects_unauthorized_and_non_positive_values() {
    let f = setup();
    let attacker = Address::generate(&f.env);
    let debt_asset = &f.debt_asset;

    assert_eq!(
        f.client
            .try_set_asset_borrow_cap(&attacker, debt_asset, &Some(10_000)),
        Err(Ok(BorrowError::Unauthorized))
    );
    assert_eq!(
        f.client.try_set_asset_borrow_cap(&f.admin, debt_asset, &Some(0)),
        Err(Ok(BorrowError::InvalidAmount))
    );
    assert_eq!(
        f.client
            .try_set_asset_borrow_cap(&f.admin, debt_asset, &Some(-500)),
        Err(Ok(BorrowError::InvalidAmount))
    );
    assert_eq!(f.client.get_asset_borrow_cap(debt_asset), None);

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
fn single_and_multi_user_borrows_enforce_per_asset_borrow_cap() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let debt_asset = &f.debt_asset;
    let col_asset = &f.collateral_asset;

    f.client
        .set_asset_borrow_cap(&f.admin, debt_asset, &Some(25_000));

    f.client.borrow(&alice, debt_asset, &15_000, col_asset, &30_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 15_000);

    // Exact cap boundary succeeds across users.
    f.client.borrow(&bob, debt_asset, &10_000, col_asset, &20_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 25_000);

    // Any additional borrow for this asset is rejected and state is preserved.
    assert_eq!(
        f.client
            .try_borrow(&bob, debt_asset, &100, col_asset, &200),
        Err(Ok(BorrowError::ExceedsBorrowCap))
    );
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 25_000);
}

#[test]
fn borrow_caps_are_isolated_per_asset() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let asset_a = Address::generate(&f.env);
    let asset_b = Address::generate(&f.env);
    let asset_c = Address::generate(&f.env);
    let col_asset = &f.collateral_asset;

    f.client
        .set_asset_borrow_cap(&f.admin, &asset_a, &Some(10_000));
    f.client
        .set_asset_borrow_cap(&f.admin, &asset_b, &Some(30_000));

    f.client.borrow(&alice, &asset_a, &10_000, col_asset, &20_000);
    assert_eq!(
        f.client
            .try_borrow(&bob, &asset_a, &100, col_asset, &200),
        Err(Ok(BorrowError::ExceedsBorrowCap))
    );

    // Asset B and uncapped Asset C remain unaffected by Asset A reaching its cap.
    f.client.borrow(&bob, &asset_b, &30_000, col_asset, &60_000);
    assert_eq!(
        f.client
            .try_borrow(&bob, &asset_b, &100, col_asset, &200),
        Err(Ok(BorrowError::ExceedsBorrowCap))
    );

    f.client.borrow(&alice, &asset_c, &50_000, col_asset, &100_000);
    assert_eq!(f.client.get_asset_total_borrowed(&asset_a), 10_000);
    assert_eq!(f.client.get_asset_total_borrowed(&asset_b), 30_000);
    assert_eq!(f.client.get_asset_total_borrowed(&asset_c), 50_000);
}

#[test]
fn repayments_and_liquidations_free_per_asset_borrow_capacity() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let liquidator = Address::generate(&f.env);
    let debt_asset = &f.debt_asset;
    let col_asset = &f.collateral_asset;

    f.client
        .set_asset_borrow_cap(&f.admin, debt_asset, &Some(20_000));

    f.client.borrow(&alice, debt_asset, &20_000, col_asset, &40_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 20_000);

    // Cap reached: new borrow blocked
    assert_eq!(
        f.client
            .try_borrow(&bob, debt_asset, &1_000, col_asset, &2_000),
        Err(Ok(BorrowError::ExceedsBorrowCap))
    );

    // Repayment frees capacity under the asset borrow cap.
    f.client.repay(&alice, debt_asset, &8_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 12_000);

    // Bob can now borrow up to the freed amount.
    f.client.borrow(&bob, debt_asset, &8_000, col_asset, &16_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 20_000);

    // Liquidation also frees capacity:
    // Drop collateral price so alice is liquidatable.
    f.oracle_client().set_price(col_asset, &(PRICE_SCALE / 5));
    f.env.ledger().with_mut(|l| l.timestamp += 1);
    let res = f.client.liquidate(&liquidator, &alice, debt_asset, col_asset, &4_000);
    assert_eq!(res.debt_repaid, 4_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 16_000);

    // Bob borrows again with the newly freed capacity.
    f.client.borrow(&bob, debt_asset, &4_000, col_asset, &8_000);
    assert_eq!(f.client.get_asset_total_borrowed(debt_asset), 20_000);
}

#[test]
fn event_emitted_on_borrow_cap_update() {
    let f = setup();
    let debt_asset = &f.debt_asset;

    f.client
        .set_asset_borrow_cap(&f.admin, debt_asset, &Some(50_000));

    let events = f.env.events().all();
    assert!(!events.events().is_empty());
}
