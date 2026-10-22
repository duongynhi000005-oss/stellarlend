use super::*;
use crate::borrow::{calculate_interest, RateType};
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    Address, Env,
};

fn setup_test(
    env: &Env,
) -> (
    LendingContractClient<'_>,
    Address,
    Address,
    Address,
    Address,
) {
    let contract_id = env.register(LendingContract, ());
    let client = LendingContractClient::new(env, &contract_id);

    let admin = Address::generate(env);
    let user = Address::generate(env);
    let asset = Address::generate(env);
    let collateral_asset = Address::generate(env);

    client.initialize(&admin, &1_000_000_000, &1000);
    (client, admin, user, asset, collateral_asset)
}

#[test]
fn new_positions_do_not_duplicate_the_legacy_storage_entry() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);

    let legacy_key = crate::borrow::BorrowDataKey::BorrowUserDebt(user);
    let has_legacy_entry = env.as_contract(&client.address, || {
        env.storage().persistent().has(&legacy_key)
    });
    assert!(!has_legacy_entry);
}

#[test]
fn test_borrow_stable_rate_choice_and_accounting() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow_with_rate(
        &user,
        &asset,
        &10_000,
        &collateral_asset,
        &20_000,
        &RateType::Stable,
    );

    let stable = client.get_user_debt_with_rate(&user, &RateType::Stable);
    assert_eq!(stable.borrowed_amount, 10_000);
    assert_eq!(stable.rate_type, RateType::Stable);
    assert!(stable.stable_rate_bps > 0);

    let variable = client.get_user_debt_with_rate(&user, &RateType::Variable);
    assert_eq!(variable.borrowed_amount, 0);
    assert_eq!(variable.interest_accrued, 0);
}

#[test]
fn test_stable_rate_recalculates_periodically_and_includes_premium() {
    let env = Env::default();
    env.mock_all_auths();

    env.ledger().with_mut(|li| {
        li.timestamp = 1000;
    });

    let (client, admin, user, asset, collateral_asset) = setup_test(&env);

    // Set initial variable rate to 5%.
    client.set_variable_borrow_rate_bps(&admin, &500);

    client.borrow_with_rate(
        &user,
        &asset,
        &10_000,
        &collateral_asset,
        &20_000,
        &RateType::Stable,
    );

    let p1 = client.get_user_debt_with_rate(&user, &RateType::Stable);
    assert_eq!(p1.stable_rate_bps, 600); // 500 avg + 100 premium

    // Advance time past default 6h recalc window, then increase variable rate.
    env.ledger().with_mut(|li| {
        li.timestamp = 1000 + 6 * 3600 + 1;
    });
    client.set_variable_borrow_rate_bps(&admin, &1000);

    // Trigger stable state update by borrowing stable with a new user.
    let user2 = Address::generate(&env);
    client.borrow_with_rate(
        &user2,
        &asset,
        &10_000,
        &collateral_asset,
        &20_000,
        &RateType::Stable,
    );

    let p2 = client.get_user_debt_with_rate(&user2, &RateType::Stable);
    // EMA: (500*9 + 1000)/10 = 550; + premium 100 = 650
    assert_eq!(p2.stable_rate_bps, 650);
}

#[test]
fn test_switch_rate_type_moves_accounting_and_charges_fee() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);

    // Switch to stable
    client.switch_rate_type(&user, &asset, &RateType::Stable);

    let variable = client.get_user_debt_with_rate(&user, &RateType::Variable);
    assert_eq!(variable.borrowed_amount, 0);
    assert_eq!(variable.interest_accrued, 0);

    let stable = client.get_user_debt_with_rate(&user, &RateType::Stable);
    assert_eq!(stable.borrowed_amount, 10_000);
    // Switch fee is 10 bps of principal = 10
    assert!(stable.interest_accrued >= 10);
}

#[test]
fn test_borrow_success() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);

    let debt = client.get_user_debt(&user);
    assert_eq!(debt.borrowed_amount, 10_000);
    assert_eq!(debt.interest_accrued, 0);

    let collateral = client.get_user_collateral(&user);
    assert_eq!(collateral.amount, 20_000);
}

#[test]
fn test_borrow_insufficient_collateral() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    let result = client.try_borrow(&user, &asset, &10_000, &collateral_asset, &10_000);
    assert_eq!(result, Err(Ok(BorrowError::InsufficientCollateral)));
}

#[test]
fn test_borrow_protocol_paused() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin, user, asset, collateral_asset) = setup_test(&env);

    client.set_pause(&admin, &PauseType::Borrow, &true);

    let result = client.try_borrow(&user, &asset, &10_000, &collateral_asset, &20_000);
    assert_eq!(result, Err(Ok(BorrowError::ProtocolPaused)));
}

#[test]
fn test_borrow_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    let result = client.try_borrow(&user, &asset, &0, &collateral_asset, &20_000);
    assert_eq!(result, Err(Ok(BorrowError::InvalidAmount)));

    let result = client.try_borrow(&user, &asset, &10_000, &collateral_asset, &0);
    assert_eq!(result, Err(Ok(BorrowError::InvalidAmount)));
}

#[test]
fn test_borrow_below_minimum() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(LendingContract, ());
    let client = LendingContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let asset = Address::generate(&env);
    let collateral_asset = Address::generate(&env);

    client.initialize(&admin, &1_000_000_000, &5000);

    let result = client.try_borrow(&user, &asset, &1000, &collateral_asset, &2000);
    assert_eq!(result, Err(Ok(BorrowError::BelowMinimumBorrow)));
}

#[test]
fn test_borrow_debt_ceiling() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(LendingContract, ());
    let client = LendingContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let asset = Address::generate(&env);
    let collateral_asset = Address::generate(&env);

    client.initialize(&admin, &50_000, &1000);

    let result = client.try_borrow(&user, &asset, &100_000, &collateral_asset, &200_000);
    assert_eq!(result, Err(Ok(BorrowError::DebtCeilingReached)));
}

#[test]
fn test_borrow_multiple_times() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);
    client.borrow(&user, &asset, &5_000, &collateral_asset, &10_000);

    let debt = client.get_user_debt(&user);
    assert_eq!(debt.borrowed_amount, 15_000);

    let collateral = client.get_user_collateral(&user);
    assert_eq!(collateral.amount, 30_000);
}

#[test]
fn test_borrow_interest_accrual() {
    let env = Env::default();
    env.mock_all_auths();

    env.ledger().with_mut(|li| {
        li.timestamp = 1000;
    });

    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);
    client.borrow(&user, &asset, &100_000, &collateral_asset, &200_000);

    env.ledger().with_mut(|li| {
        li.timestamp = 1000 + 31536000; // 1 year later
    });

    let debt = client.get_user_debt(&user);
    assert!(debt.interest_accrued > 0);
    assert!(debt.interest_accrued <= 5000); // ~5% of 100,000
}

#[test]
fn test_interest_overflow_returns_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(LendingContract, ());

    // Construct a position that will produce an interest larger than i128 when scaled
    let mut position = DebtPosition {
        borrowed_amount: i128::MAX,
        interest_accrued: 0,
        last_update: 0,
        asset: Address::generate(&env),
        rate_type: RateType::Variable,
        stable_rate_bps: 0,
    };

    // Advance time by 100 years to amplify interest (roughly borrowed * 5x at 5% APY)
    env.ledger().with_mut(|li| {
        li.timestamp = 100 * 31_536_000;
    });

    // Borrowed amount is i128::MAX, 100y at 5% should overflow i128
    let result = env.as_contract(&contract_id, || calculate_interest(&env, &position));
    assert!(matches!(result, Err(BorrowError::Overflow)));

    // Ensure callers can propagate the error; simulate accrue step
    position.last_update = 0;
    let accrue_result = env.as_contract(&contract_id, || -> Result<(), BorrowError> {
        let new_interest = calculate_interest(&env, &position)?;
        position.interest_accrued = position
            .interest_accrued
            .checked_add(new_interest)
            .ok_or(BorrowError::Overflow)?;
        Ok(())
    });
    assert!(matches!(accrue_result, Err(BorrowError::Overflow)));
}
#[test]
fn test_collateral_ratio_validation() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    // Exactly 150% collateral - should succeed
    client.borrow(&user, &asset, &10_000, &collateral_asset, &15_000);

    // Below 150% collateral - should fail
    let user2 = Address::generate(&env);
    let result = client.try_borrow(&user2, &asset, &10_000, &collateral_asset, &14_999);
    assert_eq!(result, Err(Ok(BorrowError::InsufficientCollateral)));
}

#[test]
fn test_pause_unpause() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin, user, asset, collateral_asset) = setup_test(&env);

    client.set_pause(&admin, &PauseType::Borrow, &true);
    let result = client.try_borrow(&user, &asset, &10_000, &collateral_asset, &20_000);
    assert_eq!(result, Err(Ok(BorrowError::ProtocolPaused)));

    client.set_pause(&admin, &PauseType::Borrow, &false);
    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);
}

#[test]
fn test_overflow_protection() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(LendingContract, ());
    let client = LendingContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let asset = Address::generate(&env);
    let collateral_asset = Address::generate(&env);

    client.initialize(&admin, &i128::MAX, &1000);

    // First borrow with reasonable amount
    client.borrow(&user, &asset, &1_000_000, &collateral_asset, &2_000_000);

    // Try to borrow amount that would overflow when added to existing debt
    let huge_amount = i128::MAX - 500_000;
    let huge_collateral = i128::MAX / 2; // Large but won't overflow in calculation
    let result = client.try_borrow(
        &user,
        &asset,
        &huge_amount,
        &collateral_asset,
        &huge_collateral,
    );
    assert_eq!(result, Err(Ok(BorrowError::Overflow)));
}

#[test]
fn test_debt_position_tracks_borrowed_asset_for_repay() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);

    let debt = client.get_user_debt(&user);
    assert_eq!(debt.asset, asset);

    let repay_result = client.try_repay(&user, &debt.asset, &1_000);
    assert!(repay_result.is_ok());
}

#[test]
fn test_repay_exact_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);

    // Repay exactly 10,000 (no interest accrued because no time passed)
    let repay_result = client.try_repay(&user, &asset, &10_000);
    assert_eq!(repay_result, Ok(Ok(())));

    let debt = client.get_user_debt(&user);
    assert_eq!(debt.borrowed_amount, 0);
}

#[test]
fn test_repay_under_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);

    // Repay under 10,000
    let repay_result = client.try_repay(&user, &asset, &4_000);
    assert_eq!(repay_result, Ok(Ok(())));

    let debt = client.get_user_debt(&user);
    assert_eq!(debt.borrowed_amount, 6_000);
}

#[test]
fn test_repay_over_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, user, asset, collateral_asset) = setup_test(&env);

    client.borrow(&user, &asset, &10_000, &collateral_asset, &20_000);

    // Try to repay more than borrowed, policy is to reject
    let repay_result = client.try_repay(&user, &asset, &15_000);
    assert_eq!(repay_result, Err(Ok(BorrowError::RepayAmountTooHigh)));
}

#[test]
fn test_per_asset_borrow_cap_configurable_and_enforced() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, admin, user, asset_a, collateral_asset) = setup_test(&env);
    let asset_b = Address::generate(&env);

    client.set_asset_borrow_cap(&admin, &asset_a, &Some(20_000));
    assert_eq!(client.get_asset_borrow_cap(&asset_a), Some(20_000));
    assert_eq!(client.get_asset_borrow_cap(&asset_b), None);

    // Borrow up to 20_000 succeeds
    client.borrow(&user, &asset_a, &20_000, &collateral_asset, &40_000);
    assert_eq!(client.get_asset_total_borrowed(&asset_a), 20_000);

    // Exceeding the cap fails with ExceedsBorrowCap
    let over = client.try_borrow(&user, &asset_a, &1_000, &collateral_asset, &2_000);
    assert_eq!(over, Err(Ok(BorrowError::ExceedsBorrowCap)));

    // Uncapped asset B still permits borrows
    client.borrow(&user, &asset_b, &15_000, &collateral_asset, &30_000);
    assert_eq!(client.get_asset_total_borrowed(&asset_b), 15_000);

    // Raising the cap permits further borrowing
    client.set_asset_borrow_cap(&admin, &asset_a, &Some(30_000));
    assert_eq!(client.get_asset_borrow_cap(&asset_a), Some(30_000));
    client.borrow(&user, &asset_a, &5_000, &collateral_asset, &10_000);
    assert_eq!(client.get_asset_total_borrowed(&asset_a), 25_000);
}
