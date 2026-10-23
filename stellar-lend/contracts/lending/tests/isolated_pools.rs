mod common;

use common::*;
use soroban_sdk::{testutils::Address as _, Address};
use stellarlend_lending::BorrowError;
use test_utils::snapshotless_env;

#[test]
fn admin_can_configure_and_query_asset_pools_and_debt_ceilings() {
    let f = setup();
    let asset = Address::generate(&f.env);
    let unauthorized = Address::generate(&f.env);

    // Default pool for unconfigured asset is 0 (shared core pool).
    assert_eq!(f.client.get_asset_pool(&asset), 0);
    assert_eq!(f.client.get_pool_debt_ceiling(&1), 0);
    assert_eq!(f.client.get_pool_total_debt(&1), 0);

    // Admin assigns asset to isolated pool 1.
    f.client.set_asset_pool(&f.admin, &asset, &1);
    assert_eq!(f.client.get_asset_pool(&asset), 1);

    // Admin sets pool 1 debt ceiling to 50_000.
    f.client.set_pool_debt_ceiling(&f.admin, &1, &50_000);
    assert_eq!(f.client.get_pool_debt_ceiling(&1), 50_000);

    // Non-admin cannot assign asset pool.
    assert_eq!(
        f.client.try_set_asset_pool(&unauthorized, &asset, &2),
        Err(Ok(BorrowError::Unauthorized))
    );

    // Non-admin cannot set pool debt ceiling.
    assert_eq!(
        f.client
            .try_set_pool_debt_ceiling(&unauthorized, &1, &100_000),
        Err(Ok(BorrowError::Unauthorized))
    );

    // Negative ceiling is rejected.
    assert_eq!(
        f.client.try_set_pool_debt_ceiling(&f.admin, &1, &-100),
        Err(Ok(BorrowError::InvalidAmount))
    );

    // Real signature enforcement.
    let env = snapshotless_env();
    env.mock_all_auths();
    let (client, admin) = deploy(&env);
    env.set_auths(&[]);
    let unauthed_asset = Address::generate(&env);
    assert!(client
        .try_set_asset_pool(&admin, &unauthed_asset, &1)
        .is_err());
    assert!(client
        .try_set_pool_debt_ceiling(&admin, &1, &50_000)
        .is_err());
}

#[test]
fn isolation_enforced_blocks_cross_pool_borrowing() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    let charlie = Address::generate(&f.env);

    // Shared pool assets (pool 0)
    let shared_collateral = Address::generate(&f.env);
    let shared_debt = Address::generate(&f.env);
    f.oracle_client()
        .set_price(&shared_collateral, &PRICE_SCALE);
    f.oracle_client().set_price(&shared_debt, &PRICE_SCALE);

    // Isolated pool 1 assets
    let iso1_collateral = Address::generate(&f.env);
    let iso1_debt = Address::generate(&f.env);
    f.oracle_client().set_price(&iso1_collateral, &PRICE_SCALE);
    f.oracle_client().set_price(&iso1_debt, &PRICE_SCALE);

    f.client.set_asset_pool(&f.admin, &iso1_collateral, &1);
    f.client.set_asset_pool(&f.admin, &iso1_debt, &1);

    // 1. Trying to borrow isolated asset with shared collateral is strictly rejected.
    assert_eq!(
        f.client
            .try_borrow(&alice, &iso1_debt, &1_000, &shared_collateral, &2_000),
        Err(Ok(BorrowError::IsolatedPoolMismatch))
    );

    // 2. Trying to borrow shared asset with isolated collateral is strictly rejected.
    assert_eq!(
        f.client
            .try_borrow(&bob, &shared_debt, &1_000, &iso1_collateral, &2_000),
        Err(Ok(BorrowError::IsolatedPoolMismatch))
    );

    // 3. Borrowing within the same isolated pool succeeds.
    let res = f
        .client
        .try_borrow(&charlie, &iso1_debt, &1_000, &iso1_collateral, &2_000);
    assert!(
        res.is_ok(),
        "borrowing within same isolated pool must succeed"
    );
    assert_eq!(f.client.get_pool_total_debt(&1), 1_000);
}

#[test]
fn isolated_pool_debt_ceiling_enforced_and_freed_by_repayment() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);

    let iso_collateral = Address::generate(&f.env);
    let iso_debt = Address::generate(&f.env);
    f.oracle_client().set_price(&iso_collateral, &PRICE_SCALE);
    f.oracle_client().set_price(&iso_debt, &PRICE_SCALE);

    f.client.set_asset_pool(&f.admin, &iso_collateral, &1);
    f.client.set_asset_pool(&f.admin, &iso_debt, &1);
    f.client.set_pool_debt_ceiling(&f.admin, &1, &10_000);

    // Alice borrows 6_000 -> succeeds.
    f.client
        .borrow(&alice, &iso_debt, &6_000, &iso_collateral, &12_000);
    assert_eq!(f.client.get_pool_total_debt(&1), 6_000);

    // Bob tries to borrow 5_000 -> total 11_000 exceeds ceiling 10_000.
    assert_eq!(
        f.client
            .try_borrow(&bob, &iso_debt, &5_000, &iso_collateral, &10_000),
        Err(Ok(BorrowError::DebtCeilingReached))
    );

    // Alice repays 2_000 -> pool total debt decrements to 4_000.
    f.client.repay(&alice, &iso_debt, &2_000);
    assert_eq!(f.client.get_pool_total_debt(&1), 4_000);

    // Bob can now borrow 5_000 -> total becomes 9_000 <= 10_000.
    assert!(f
        .client
        .try_borrow(&bob, &iso_debt, &5_000, &iso_collateral, &10_000)
        .is_ok());
    assert_eq!(f.client.get_pool_total_debt(&1), 9_000);
}

#[test]
fn liquidations_strictly_scoped_to_isolated_pool() {
    let f = setup();
    let borrower = Address::generate(&f.env);
    let liquidator = Address::generate(&f.env);

    let shared_collateral = Address::generate(&f.env);
    let iso_collateral = Address::generate(&f.env);
    let iso_debt = Address::generate(&f.env);

    f.oracle_client()
        .set_price(&shared_collateral, &PRICE_SCALE);
    f.oracle_client().set_price(&iso_collateral, &PRICE_SCALE);
    f.oracle_client().set_price(&iso_debt, &PRICE_SCALE);

    f.client.set_asset_pool(&f.admin, &iso_collateral, &1);
    f.client.set_asset_pool(&f.admin, &iso_debt, &1);

    // Borrower deposits collateral in shared pool as well.
    f.client
        .deposit_collateral(&borrower, &shared_collateral, &50_000);

    // Borrower takes loan in isolated pool 1.
    f.client
        .borrow(&borrower, &iso_debt, &1_000, &iso_collateral, &1_500);

    // Price of isolated collateral drops significantly, making position unhealthy.
    let half_price = PRICE_SCALE / 2;
    f.oracle_client().set_price(&iso_collateral, &half_price);

    // Liquidator attempts cross-pool liquidation: seize shared collateral for isolated debt -> REJECTED.
    assert_eq!(
        f.client
            .try_liquidate(&liquidator, &borrower, &iso_debt, &shared_collateral, &500),
        Err(Ok(BorrowError::IsolatedPoolMismatch))
    );

    // Liquidator performs scoped liquidation seizing isolated collateral -> SUCCEEDS.
    let liq_res = f
        .client
        .liquidate(&liquidator, &borrower, &iso_debt, &iso_collateral, &500);
    assert_eq!(liq_res.debt_repaid, 500);
    assert!(liq_res.collateral_seized > 0);

    // Pool debt properly decremented after liquidation.
    assert_eq!(f.client.get_pool_total_debt(&1), 500);
}

#[test]
fn isolated_pool_health_factor_scoped_to_pool() {
    let f = setup();
    let user = Address::generate(&f.env);

    let shared_collateral = Address::generate(&f.env);
    let iso_collateral = Address::generate(&f.env);
    let iso_debt = Address::generate(&f.env);

    f.oracle_client()
        .set_price(&shared_collateral, &PRICE_SCALE);
    f.oracle_client().set_price(&iso_collateral, &PRICE_SCALE);
    f.oracle_client().set_price(&iso_debt, &PRICE_SCALE);

    f.client.set_asset_pool(&f.admin, &iso_collateral, &2);
    f.client.set_asset_pool(&f.admin, &iso_debt, &2);

    // User deposits large collateral in shared pool (100_000 USD).
    f.client
        .deposit_collateral(&user, &shared_collateral, &100_000);

    // User borrows in isolated pool 2.
    f.client
        .borrow(&user, &iso_debt, &1_000, &iso_collateral, &2_000);

    // Health factor for pool 2 should reflect only pool 2 collateral.
    let hf_pool2 = f.client.get_pool_health_factor(&user, &2);
    assert!(hf_pool2 >= 10_000, "healthy within pool 2");

    // Drop price of pool 2 collateral:
    f.oracle_client()
        .set_price(&iso_collateral, &(PRICE_SCALE / 4));

    let hf_pool2_dropped = f.client.get_pool_health_factor(&user, &2);
    assert!(
        hf_pool2_dropped < 10_000,
        "unhealthy in pool 2 despite huge shared collateral"
    );
}

#[test]
fn multiple_isolated_pools_stay_strictly_partitioned() {
    let f = setup();
    let user = Address::generate(&f.env);

    let p1_collat = Address::generate(&f.env);
    let p1_debt = Address::generate(&f.env);
    let p2_collat = Address::generate(&f.env);
    let p2_debt = Address::generate(&f.env);

    f.oracle_client().set_price(&p1_collat, &PRICE_SCALE);
    f.oracle_client().set_price(&p1_debt, &PRICE_SCALE);
    f.oracle_client().set_price(&p2_collat, &PRICE_SCALE);
    f.oracle_client().set_price(&p2_debt, &PRICE_SCALE);

    f.client.set_asset_pool(&f.admin, &p1_collat, &1);
    f.client.set_asset_pool(&f.admin, &p1_debt, &1);
    f.client.set_asset_pool(&f.admin, &p2_collat, &2);
    f.client.set_asset_pool(&f.admin, &p2_debt, &2);

    // Cannot borrow Pool 2 debt using Pool 1 collateral.
    assert_eq!(
        f.client
            .try_borrow(&user, &p2_debt, &1_000, &p1_collat, &2_000),
        Err(Ok(BorrowError::IsolatedPoolMismatch))
    );

    // Cannot borrow Pool 1 debt using Pool 2 collateral.
    assert_eq!(
        f.client
            .try_borrow(&user, &p1_debt, &1_000, &p2_collat, &2_000),
        Err(Ok(BorrowError::IsolatedPoolMismatch))
    );
}

#[test]
fn sweep_debt_dust_restores_isolated_pool_capacity() {
    let f = setup();
    let user = Address::generate(&f.env);

    let iso_collat = Address::generate(&f.env);
    let iso_debt = Address::generate(&f.env);

    f.oracle_client().set_price(&iso_collat, &PRICE_SCALE);
    f.oracle_client().set_price(&iso_debt, &PRICE_SCALE);

    f.client.set_asset_pool(&f.admin, &iso_collat, &1);
    f.client.set_asset_pool(&f.admin, &iso_debt, &1);

    // Borrow minimum borrow amount (100).
    f.client.borrow(&user, &iso_debt, &100, &iso_collat, &300);
    assert_eq!(f.client.get_pool_total_debt(&1), 100);

    // Repay 50, leaving 50 dust.
    f.client.repay(&user, &iso_debt, &50);
    assert_eq!(f.client.get_pool_total_debt(&1), 50);

    // Sweep dust.
    let swept = f.client.sweep_debt_dust(&user, &iso_debt);
    assert_eq!(swept, 50);
    assert_eq!(f.client.get_pool_total_debt(&1), 0);
}
