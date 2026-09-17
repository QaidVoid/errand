//! Tests for the usage window, ported from `usage_test.ts` and the usage
//! halves of `zai_test.ts`.

use std::future::Future;

use tokio::sync::mpsc;

use super::{
    QUOTA_TTL_MS, Quota, QuotaGate, STATUS_LIMIT, Window, is_spent, quota_message, spent_message,
    usage_status,
};

fn window(provider: &str, percentage: f64, relative: Option<&str>) -> Window {
    Window {
        provider: provider.to_owned(),
        quota: Quota {
            percentage,
            resets_at: None,
        },
        relative: relative.map(str::to_owned),
    }
}

fn quota(percentage: f64, resets_at: Option<i64>) -> Quota {
    Quota {
        percentage,
        resets_at,
    }
}

/// One provider reads as a sentence: there is room, and naming it says nothing.
#[test]
fn a_single_provider_is_not_named() {
    assert_eq!(
        usage_status(&[window("zai-coding-cn", 12.0, Some("in 2h"))]),
        Some("88% usage left, resets in 2h".to_owned())
    );
    assert_eq!(
        usage_status(&[window("zai-coding-cn", 12.0, None)]),
        Some("88% usage left".to_owned())
    );
    assert_eq!(
        usage_status(&[window("zai-coding-cn", 100.0, Some("in 30m"))]),
        Some("usage spent, back in 30m".to_owned())
    );
}

/// Several are named, because which one has room is then the whole question.
#[test]
fn several_providers_are_named_and_cut_to_what_is_left() {
    assert_eq!(
        usage_status(&[
            window("zai-coding-cn", 12.0, Some("in 2h")),
            window("ajamxhacker", 0.0, Some("in 5h")),
        ]),
        Some("zai-coding-cn 88% | ajamxhacker 100%".to_owned())
    );
    // A spent one says when it is back, which is all that is left to know.
    assert_eq!(
        usage_status(&[
            window("zai", 100.0, Some("in 30m")),
            window("meta", 40.0, None)
        ]),
        Some("zai spent, back in 30m | meta 60%".to_owned())
    );
}

/// A host asking two must not lose the answer it has because the other failed.
#[test]
fn a_provider_that_did_not_answer_is_simply_absent() {
    assert_eq!(
        usage_status(&[window("meta", 25.0, None)]),
        Some("75% usage left".to_owned())
    );
    assert_eq!(usage_status(&[]), None);
}

/// The service refuses a long status silently, so it is trimmed here.
#[test]
fn the_status_is_kept_inside_what_the_service_accepts() {
    let many: Vec<Window> = (0..12)
        .map(|index| {
            window(
                &format!("provider-with-a-long-name-{index}"),
                f64::from(index),
                Some("in 3h"),
            )
        })
        .collect();
    let status = usage_status(&many).expect("some windows fit");

    assert!(status.len() <= STATUS_LIMIT, "status was {}", status.len());
    // Dropped whole rather than truncated: half a name reads as another provider.
    assert!(!status.ends_with('|'));
    assert!(status.contains("provider-with-a-long-name-0"));
}

#[tokio::test]
async fn an_unspent_window_is_reused_briefly_then_asked_about_again() {
    let (signals, mut seen) = mpsc::unbounded_channel::<()>();
    let now = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1_000));
    let clock = std::sync::Arc::clone(&now);
    let mut gate = QuotaGate::new(
        move || {
            let signals = signals.clone();
            async move {
                let _ = signals.send(());
                Some(quota(20.0, Some(9_999_999)))
            }
        },
        move || clock.load(std::sync::atomic::Ordering::Relaxed),
    );

    let _ = gate.current().await;
    let _ = gate.current().await;
    assert_eq!(seen.recv().await, Some(()));
    assert!(seen.try_recv().is_err());

    now.fetch_add(QUOTA_TTL_MS + 1, std::sync::atomic::Ordering::Relaxed);
    let _ = gate.current().await;
    assert_eq!(seen.recv().await, Some(()));
}

/// Nothing can change before it rolls over, so there is nothing to ask.
#[tokio::test]
async fn a_spent_window_is_not_asked_about_again_until_it_resets() {
    let (signals, mut seen) = mpsc::unbounded_channel::<()>();
    let now = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1_000));
    let clock = std::sync::Arc::clone(&now);
    let mut gate = QuotaGate::new(
        move || {
            let signals = signals.clone();
            async move {
                let _ = signals.send(());
                Some(quota(100.0, Some(500_000)))
            }
        },
        move || clock.load(std::sync::atomic::Ordering::Relaxed),
    );

    let _ = gate.current().await;
    now.fetch_add(400_000, std::sync::atomic::Ordering::Relaxed);
    let _ = gate.current().await;
    assert_eq!(seen.recv().await, Some(()));
    assert!(seen.try_recv().is_err());

    now.store(500_001, std::sync::atomic::Ordering::Relaxed);
    let _ = gate.current().await;
    assert_eq!(seen.recv().await, Some(()));
}

#[tokio::test]
async fn an_answer_that_could_not_be_had_is_not_held_on_to() {
    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let counter = std::rc::Rc::clone(&asked);
    let mut gate = QuotaGate::new(
        move || {
            let counter = std::rc::Rc::clone(&counter);
            async move {
                counter.set(counter.get() + 1);
                None::<Quota>
            }
        },
        || 1_000,
    );

    assert_eq!(gate.current().await, None);
    assert_eq!(gate.current().await, None);
    assert_eq!(asked.get(), 2);
}

#[tokio::test]
async fn forgetting_makes_the_next_question_reach_the_provider() {
    let (signals, mut seen) = mpsc::unbounded_channel::<()>();
    let mut gate = QuotaGate::new(
        move || {
            let signals = signals.clone();
            async move {
                let _ = signals.send(());
                Some(quota(20.0, Some(9_999_999)))
            }
        },
        || 1_000,
    );

    let _ = gate.current().await;
    gate.forget();
    let _ = gate.current().await;

    assert_eq!(seen.recv().await, Some(()));
}

/// The number somebody is deciding on is what is left, not what is spent.
#[test]
fn the_usage_line_says_what_is_left_and_when_it_comes_back() {
    let line = quota_message(
        "the provider",
        &Quota {
            percentage: 42.4,
            resets_at: Some(0),
        },
        Some("in 2 hours"),
    );

    assert!(line.contains("58% of the provider's usage window is left"));
    assert!(line.contains("resets in 2 hours"));
}

#[test]
fn a_spent_window_says_so_rather_than_saying_zero_percent_is_left() {
    let line = quota_message(
        "the provider",
        &Quota {
            percentage: 100.0,
            resets_at: Some(0),
        },
        Some("in 10 minutes"),
    );

    assert!(line.contains("is spent"));
    assert!(line.contains("in 10 minutes"));
}

#[test]
fn a_refusal_says_when_to_come_back() {
    assert!(spent_message("the provider", Some("in 3 hours")).contains("resets in 3 hours"));
}

#[test]
fn a_window_is_spent_only_once_it_is_all_the_way_spent() {
    assert!(!is_spent(&Quota {
        percentage: 99.9,
        resets_at: Some(0),
    }));
    assert!(is_spent(&Quota {
        percentage: 100.0,
        resets_at: Some(0),
    }));
    assert!(is_spent(&Quota {
        percentage: 140.0,
        resets_at: Some(0),
    }));
}

/// A reading the gate never asks about; kept so the future shapes stay honest.
#[allow(dead_code)]
fn _pending_read() -> impl Future<Output = Option<Quota>> {
    std::future::pending()
}
