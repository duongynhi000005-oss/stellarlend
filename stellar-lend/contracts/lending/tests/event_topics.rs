//! Pins the on-chain event layout the off-chain indexer depends on
//! (`api/src/services/eventIndex`, issue #685).
//!
//! The indexer derives an event's `type` from its first topic (the snake_case
//! struct name minus `_event`), its `actor` from the first address topic, and
//! reads `amount` / `asset` from the map payload. Renaming an event struct,
//! moving a `#[topic]` field, or changing the payload format silently breaks
//! indexing, replay and analytics — so this suite fails first.

mod common;

use common::*;
use soroban_sdk::testutils::{Address as _, Events as _, Ledger as _};
use soroban_sdk::xdr::{ContractEventBody, ScAddress, ScVal};
use soroban_sdk::Address;

#[derive(Debug)]
struct EmittedEvent {
    name: String,
    address_topics: usize,
    data_keys: Vec<String>,
}

fn last_call_events(f: &Fixture) -> Vec<EmittedEvent> {
    f.env
        .events()
        .all()
        .events()
        .iter()
        .map(|e| {
            let ContractEventBody::V0(body) = &e.body;
            let name = match body.topics.first() {
                Some(ScVal::Symbol(s)) => s.to_string(),
                other => panic!("first topic must be the event name symbol, got {other:?}"),
            };
            let address_topics = body
                .topics
                .iter()
                .skip(1)
                .filter(|t| {
                    matches!(
                        t,
                        ScVal::Address(ScAddress::Account(_) | ScAddress::Contract(_))
                    )
                })
                .count();
            let data_keys = match &body.data {
                ScVal::Map(Some(map)) => map
                    .iter()
                    .filter_map(|entry| match &entry.key {
                        ScVal::Symbol(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .collect(),
                other => panic!(
                    "{name}: payload must be a map (indexer reads named fields), got {other:?}"
                ),
            };
            EmittedEvent {
                name,
                address_topics,
                data_keys,
            }
        })
        .collect()
}

fn expect_event(events: &[EmittedEvent], name: &str, fields: &[&str]) {
    let e = events
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("expected `{name}` event, got {events:?}"));
    assert!(
        e.address_topics >= 1,
        "{name}: user address must be a topic (indexer actor), got {e:?}"
    );
    for field in fields {
        assert!(
            e.data_keys.iter().any(|k| k == field),
            "{name}: payload missing `{field}`: {e:?}"
        );
    }
}

#[test]
fn lending_entry_points_emit_indexable_events() {
    let f = setup();
    let user = Address::generate(&f.env);
    let (ca, da) = (&f.collateral_asset, &f.debt_asset);

    f.client.deposit(&user, ca, &10_000);
    expect_event(
        &last_call_events(&f),
        "vault_deposit_event",
        &["asset", "amount"],
    );

    f.client.borrow(&user, da, &1_000, ca, &1_500);
    expect_event(&last_call_events(&f), "borrow_event", &["asset", "amount"]);

    f.client.repay(&user, da, &1_000);
    expect_event(&last_call_events(&f), "repay_event", &["asset", "amount"]);

    f.client.withdraw(&user, ca, &10_000);
    expect_event(
        &last_call_events(&f),
        "withdraw_event",
        &["asset", "amount"],
    );

    f.client.deposit_collateral(&user, ca, &500);
    expect_event(
        &last_call_events(&f),
        "borrow_collateral_deposit_event",
        &["asset", "amount"],
    );
}

#[test]
fn liquidation_and_risk_config_emit_indexable_events() {
    let f = setup();
    let (ca, da) = (&f.collateral_asset, &f.debt_asset);
    let borrower = Address::generate(&f.env);
    let liquidator = Address::generate(&f.env);

    f.client.set_liquidation_incentive_bps(&f.admin, &800);
    expect_event(
        &last_call_events(&f),
        "liq_incentive_updated_event",
        &["previous_bps", "new_bps"],
    );

    f.client
        .set_asset_liq_incentive_bps(&f.admin, ca, &Some(600));
    expect_event(
        &last_call_events(&f),
        "liq_incentive_updated_event",
        &["asset", "previous_bps", "new_bps"],
    );

    f.client.set_interest_rate_config(
        &f.admin,
        &stellarlend_lending::InterestRateConfigUpdate {
            base_rate_bps: Some(400),
            kink_utilization_bps: None,
            slope_bps: None,
            jump_slope_bps: None,
            rate_floor_bps: None,
            rate_ceiling_bps: None,
            spread_bps: None,
        },
    );
    expect_event(
        &last_call_events(&f),
        "rate_config_updated_event",
        &["base_rate_bps", "slope_bps"],
    );

    f.client.borrow(&borrower, da, &1_000, ca, &1_500);
    f.oracle_client().set_price(ca, &(PRICE_SCALE / 2));
    f.env.ledger().with_mut(|l| l.timestamp += 1);
    f.client.liquidate(&liquidator, &borrower, da, ca, &100);
    expect_event(
        &last_call_events(&f),
        "liquidation_event",
        &["debt_liquidated", "collateral_seized", "incentive_amount"],
    );
}
