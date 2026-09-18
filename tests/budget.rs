use gray_discord::budget::{Budget, BudgetBlocked};
use serde_json::json;
use std::path::Path;

fn policy(tmp: &str, daily: f64, turn: f64) -> serde_json::Value {
    let _ = tmp;
    json!({"daily_usd": daily, "turn_usd": turn, "input_per_million": 1.0, "output_per_million": 2.0, "model": "fixture"})
}

#[test]
fn reservations_survive_restart_and_never_treat_unknown_as_free() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("budget.sqlite");
    let budget = Budget::new(&path).unwrap();
    let p = policy("", 1.0, 0.6);
    assert!(matches!(
        budget.reserve("unknown", &serde_json::Value::Null, "fixture"),
        Err(BudgetBlocked(_))
    ));
    assert!(matches!(
        budget.reserve("wrong-model", &p, "other"),
        Err(BudgetBlocked(_))
    ));
    budget.reserve("one", &p, "fixture").unwrap();
    assert!(matches!(
        Budget::new(&path).unwrap().reserve("two", &p, "fixture"),
        Err(BudgetBlocked(_))
    ));
    budget.settle(
        "one",
        &json!({"cost_micros": 100000, "usage_complete": true}),
    );
    budget.reserve("two", &p, "fixture").unwrap();
    budget.settle("two", &json!({"cost_micros": 0, "usage_complete": false}));
    assert_eq!(budget.total(), 700000);
}

#[test]
fn invalid_prices_fail_closed_and_overspend_is_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let budget = Budget::new(&tmp.path().join("ledger.sqlite")).unwrap();
    let base = json!({"daily_usd": 1.0, "turn_usd": 0.5, "input_per_million": 1.0, "output_per_million": 1.0, "model": "fixture"});
    for bad in [json!(-1.0), json!(f64::NAN), json!(f64::INFINITY)] {
        let mut p = base.clone();
        p["output_per_million"] = bad;
        assert!(matches!(
            budget.reserve("bad", &p, "fixture"),
            Err(BudgetBlocked(_))
        ));
    }
    budget.reserve("one", &base, "fixture").unwrap();
    budget.settle(
        "one",
        &json!({"cost_micros": 1500000, "usage_complete": true}),
    );
    assert_eq!(budget.total(), 1500000);
    assert!(matches!(
        budget.reserve("two", &base, "fixture"),
        Err(BudgetBlocked(_))
    ));
}

#[test]
fn unsettled_old_reservations_still_block_new_day() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ledger.sqlite");
    let budget = Budget::new(&path).unwrap();
    let p = json!({"daily_usd": 1.0, "turn_usd": 0.6, "input_per_million": 1.0, "output_per_million": 1.0, "model": "fixture"});
    budget.reserve("yesterday", &p, "fixture").unwrap();
    {
        let conn = rusqlite::Connection::open(Path::new(&path)).unwrap();
        conn.execute("UPDATE reservations SET day='2000-01-01'", [])
            .unwrap();
    }
    assert!(matches!(
        budget.reserve("today", &p, "fixture"),
        Err(BudgetBlocked(_))
    ));
}
