//! Engine startup invariants with an isolated, empty configuration.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cutoffharness_config::{LoadOptions, Loaded, load_with};
use cutoffharness_core::Engine;
use cutoffharness_core::protocol::{Event, InputItem, LoadState, Op, Subsystem, TurnOverrides};

fn isolated(root: &Path) -> Loaded {
    let mut vars: HashMap<String, OsString> = HashMap::new();
    for (k, sub) in [
        ("CUTOFFHARNESS_SYSTEM_DIR", "system"),
        ("CUTOFFHARNESS_CONFIG_DIR", "user"),
        ("CUTOFFHARNESS_DATA_DIR", "data"),
        ("CUTOFFHARNESS_CACHE_DIR", "cache"),
    ] {
        vars.insert(k.into(), root.join(sub).into_os_string());
    }
    vars.insert("CUTOFFHARNESS_DEFAULTS_DIR".into(), "none".into());
    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).expect("mkdir");
    load_with(&move |k| vars.get(k).cloned(), &LoadOptions::new(workspace)).expect("load")
}

async fn next(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event within 5s")
        .expect("channel open")
}

#[tokio::test]
async fn empty_config_starts_cleanly_with_no_builtins() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::spawn(Arc::new(isolated(tmp.path())));

    let mut provider = None;
    let mut config = None;
    loop {
        match next(&mut engine.events).await {
            Event::StartupProgress {
                subsystem: Subsystem::Provider,
                state,
            } => provider = Some(state),
            Event::StartupProgress {
                subsystem: Subsystem::Config,
                state,
            } => config = Some(state),
            Event::StartupComplete => break,
            _ => {}
        }
    }
    assert!(
        matches!(config, Some(LoadState::Ready { ref detail }) if detail.contains("0 providers")),
        "{config:?}"
    );
    assert!(
        matches!(provider, Some(LoadState::NeedsAttention { ref detail }) if detail.contains("no providers configured")),
        "{provider:?}"
    );

    // A turn still completes (stub engine until adapters land), and interrupt is honoured.
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("hi")],
            overrides: TurnOverrides::default(),
        })
        .await;
    engine.submit(Op::Interrupt).await;
    loop {
        match next(&mut engine.events).await {
            Event::TurnAborted { .. } | Event::TurnComplete { .. } => break,
            _ => {}
        }
    }
    engine.shutdown().await;
}

#[tokio::test]
async fn turns_submitted_during_startup_are_queued() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::spawn(Arc::new(isolated(tmp.path())));
    engine
        .submit(Op::UserTurn {
            items: vec![InputItem::text("early")],
            overrides: TurnOverrides::default(),
        })
        .await;
    let mut queued = false;
    loop {
        match next(&mut engine.events).await {
            Event::TurnQueued { .. } => queued = true,
            Event::TurnComplete { .. } => break,
            Event::TurnStarted { .. } => assert!(queued, "turn started before startup finished"),
            _ => {}
        }
    }
    engine.shutdown().await;
}
