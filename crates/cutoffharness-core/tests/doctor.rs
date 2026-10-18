//! `doctor` against a mock provider.

mod common;

use std::sync::Arc;

use cutoffharness_core::doctor;
use cutoffharness_core::protocol::CheckStatus;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn reports_each_provider_and_the_local_checks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": [{"id": "tiny"}, {"id": "big"}]})),
        )
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let loaded = common::workspace(tmp.path(), &server.uri(), "[]", "");
    let checks = doctor::run(Arc::new(loaded)).await;
    let get = |name: &str| {
        checks
            .iter()
            .find(|c| c.name.trim() == name)
            .unwrap_or_else(|| panic!("no {name} check in {checks:?}"))
    };
    assert_eq!(get("config").status, CheckStatus::Ok);
    assert_eq!(get("providers").status, CheckStatus::Ok);
    assert!(
        get("mock").detail.starts_with("2 models"),
        "{:?}",
        get("mock")
    );
    assert_eq!(get("sessions").status, CheckStatus::Ok);
    assert_eq!(get("defaults").status, CheckStatus::Info);
    let names: Vec<&str> = checks.iter().map(|c| c.name.trim()).collect();
    assert_eq!(&names[..4], ["config", "defaults", "providers", "mock"]);
}
