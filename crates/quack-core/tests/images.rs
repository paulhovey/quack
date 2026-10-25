//! Images: `view_image` sends the pixels with a caption, attached paths ride the user
//! message, a model without vision gets neither, and a resume replays them exactly.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{bodies, sse_call, sse_text, until_done};
use quack_core::protocol::{Event, InputItem, Op, TurnOverrides};
use quack_core::{Engine, EngineHandle, Resume};
use serde_json::json;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

/// A 2x1 PNG header, enough for the probe and the size limit.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R', 0, 0, 0,
    2, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0x7D, 0x0C, 0xC0, 0xFA,
];
const VISION: &str = r#"[{ id = "tiny", vision = true }]"#;

async fn engine(root: &Path, server: &MockServer, models: &str) -> EngineHandle {
    let loaded = common::workspace(root, &server.uri(), models, "");
    Engine::spawn(Arc::new(loaded))
}

/// Step 1 calls `view_image` on `pic.png`; anything after a tool result is done.
async fn view_image_script(server: &MockServer) {
    Mock::given(method("POST"))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(sse_text("done"))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_call("c1", "view_image", json!({ "path": "pic.png" })))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn tool_names(body: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(body).expect("json");
    v["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn view_image_sends_the_pixels_after_the_tool_result() {
    let server = MockServer::start().await;
    view_image_script(&server).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = engine(tmp.path(), &server, VISION).await;
    std::fs::write(tmp.path().join("ws").join("pic.png"), PNG).expect("png");

    common::turn(&mut engine, "what is in the picture?").await;
    let bodies = bodies(&server).await;
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    let names = tool_names(&bodies[0]);
    assert!(names.contains(&"view_image".to_owned()), "{names:?}");
    let second = &bodies[1];
    // The caption stays in the tool result...
    let v: serde_json::Value = serde_json::from_str(second).expect("json");
    let messages = v["messages"].as_array().expect("messages");
    let tool_at = messages
        .iter()
        .position(|m| {
            m["role"] == "tool"
                && m["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("[image: pic.png"))
        })
        .expect("tool message carrying the caption");
    // ...and the pixels follow it as user-content image parts.
    messages
        .iter()
        .skip(tool_at + 1)
        .find(|m| {
            m["role"] == "user"
                && m["content"].as_array().is_some_and(|parts| {
                    parts.iter().any(|p| {
                        p["type"] == "image_url"
                            && p["image_url"]["url"]
                                .as_str()
                                .is_some_and(|u| u.starts_with("data:image/png;base64,"))
                    })
                })
        })
        .expect("image parts ride a user message after the tool result");
}

#[tokio::test]
async fn attached_images_ride_the_user_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("seen"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = engine(tmp.path(), &server, VISION).await;
    std::fs::write(tmp.path().join("ws").join("shot.png"), PNG).expect("png");

    engine
        .submit(Op::UserTurn {
            items: vec![
                InputItem::Image {
                    path: "shot.png".into(),
                },
                InputItem::text("describe it"),
            ],
            overrides: TurnOverrides::default(),
        })
        .await;
    until_done(&mut engine).await;
    let body = bodies(&server).await.remove(0);
    assert!(body.contains("data:image/png;base64,"), "{body}");
    assert!(body.contains("describe it"), "{body}");
}

#[tokio::test]
async fn a_model_without_vision_gets_neither_the_tool_nor_the_pixels() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_text("done"))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = engine(tmp.path(), &server, r#"["tiny"]"#).await;
    std::fs::write(tmp.path().join("ws").join("shot.png"), PNG).expect("png");

    engine
        .submit(Op::UserTurn {
            items: vec![
                InputItem::Image {
                    path: "shot.png".into(),
                },
                InputItem::text("describe it"),
            ],
            overrides: TurnOverrides::default(),
        })
        .await;
    let seen = until_done(&mut engine).await;
    let body = bodies(&server).await.remove(0);
    let names = tool_names(&body);
    assert!(!names.contains(&"view_image".to_owned()), "{names:?}");
    assert!(!body.contains("base64"), "{body}");
    assert!(
        seen.iter().any(|e| matches!(e, Event::Warning { .. })),
        "the missing vision is warned about"
    );
}

#[tokio::test]
async fn a_resume_replays_the_image() {
    let server = MockServer::start().await;
    view_image_script(&server).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = engine(tmp.path(), &server, VISION).await;
    std::fs::write(tmp.path().join("ws").join("pic.png"), PNG).expect("png");
    common::turn(&mut engine, "look").await;
    drop(engine);

    let loaded = common::workspace(tmp.path(), &server.uri(), VISION, "");
    let mut second = Engine::spawn_with(Arc::new(loaded), Resume::Latest);
    let mut resumed = false;
    while let Ok(e) =
        tokio::time::timeout(std::time::Duration::from_secs(10), second.events.recv()).await
    {
        if let Ok(Event::SessionResumed { .. }) = e {
            resumed = true;
            break;
        }
    }
    assert!(resumed, "session resumed");
    Mock::given(method("POST"))
        .and(body_string_contains("and again"))
        .respond_with(sse_text("ok"))
        .mount(&server)
        .await;
    common::turn(&mut second, "and again").await;
    let last = bodies(&server).await.pop().expect("requests");
    assert!(
        last.contains("data:image/png;base64,"),
        "the replayed history still carries the image"
    );
}

#[tokio::test]
async fn a_model_without_vision_gets_a_note_in_place_of_earlier_images() {
    let server = MockServer::start().await;
    view_image_script(&server).await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut engine = engine(tmp.path(), &server, VISION).await;
    std::fs::write(tmp.path().join("ws").join("pic.png"), PNG).expect("png");
    common::turn(&mut engine, "look").await;
    drop(engine);

    // The same session resumed on a model that takes no images.
    let loaded = common::workspace(tmp.path(), &server.uri(), r#"["tiny"]"#, "");
    let mut second = Engine::spawn_with(Arc::new(loaded), Resume::Latest);
    let mut resumed = false;
    while let Ok(e) =
        tokio::time::timeout(std::time::Duration::from_secs(10), second.events.recv()).await
    {
        if let Ok(Event::SessionResumed { .. }) = e {
            resumed = true;
            break;
        }
    }
    assert!(resumed, "session resumed");
    common::turn(&mut second, "and again").await;
    let last = bodies(&server).await.pop().expect("requests");
    assert!(last.contains("and again"), "{last}");
    assert!(
        !last.contains("base64"),
        "no pixels go to a model without vision"
    );
    assert!(last.contains("1 image not sent"), "{last}");
}
