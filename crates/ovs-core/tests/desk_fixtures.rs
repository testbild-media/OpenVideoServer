//! Every desk's fixture types, from the status page, without signing in.
//!
//! The owner, 2026-10-19: "können wir auf dem Server der Admin GUI/Status Seite
//! noch alle Fixtures als Zip mitliefern?" What is held here is the promise the
//! page makes: the file it offers is there, is a ZIP, is called what the page
//! says, and is as big as the page says.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ovs_core::{router, AppState};
use ovs_db::Database;
use tower::ServiceExt;

async fn fetch(app: &axum::Router, path: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let request = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("reads")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn the_status_page_offers_the_fixture_zip_and_the_server_hands_it_out() {
    let app = router(AppState::new(
        Database::open_in_memory().expect("opens"),
        "0.0.1-test",
    ));

    let (status, _, body) = fetch(&app, "/status.json").await;
    assert_eq!(status, StatusCode::OK);
    let page: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let fixtures = &page["fixtures"];
    assert_eq!(fixtures["file"], "OpenVideoServer-0.0.1-test-fixtures.zip");
    assert_eq!(fixtures["desks"][0]["name"], "grandMA1");
    assert_eq!(
        fixtures["desks"][0]["fixture_types"],
        ovs_proto::grandma1::fixture_files().len()
    );

    // No session, as the client download has none.
    let (status, headers, zip) = fetch(&app, "/fixtures.zip").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/zip");
    assert_eq!(
        headers["content-disposition"],
        "attachment; filename=\"OpenVideoServer-0.0.1-test-fixtures.zip\""
    );
    assert_eq!(fixtures["bytes"], zip.len());
    assert_eq!(&zip[..4], b"PK\x03\x04", "not a ZIP");
    let master = b"grandMA1/TESTBILD MEDIA@OVS MASTER.TXT";
    assert!(
        zip.windows(master.len()).any(|window| window == master),
        "the master is not in its desk's folder"
    );
}
