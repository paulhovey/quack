//! Editing a config file during a session is noticed, with `/reload` to apply it.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::workspace;
use quack_core::Engine;
use quack_core::protocol::Event;

#[tokio::test]
async fn an_edited_config_file_is_noticed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = workspace(tmp.path(), "http://127.0.0.1:9", "[\"tiny\"]", "");
    let user = loaded.dirs.user_config.clone();
    let mut engine = Engine::spawn(Arc::new(loaded));
    // Let the watcher take its first stamp, then change a file it watches.
    tokio::time::sleep(Duration::from_millis(500)).await;
    std::fs::write(user.join("config.toml"), "[ui]\nascii = true\n").expect("write");
    let notice = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match engine.events.recv().await {
                Ok(Event::Notice { message }) if message.contains("/reload") => break message,
                Ok(_) => {}
                Err(e) => panic!("events ended: {e}"),
            }
        }
    })
    .await
    .expect("noticed within 15s");
    assert!(notice.contains("config files changed"), "{notice}");
    engine.shutdown().await;
}
