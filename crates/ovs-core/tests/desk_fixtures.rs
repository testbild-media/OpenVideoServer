//! Every desk's fixture types, from the status page, without signing in.
//!
//! The owner, 2026-10-19: "können wir auf dem Server der Admin GUI/Status Seite
//! noch alle Fixtures als Zip mitliefern?" — and on 2026-11-09, after the
//! server leaked a fixture catalogue every time the System screen asked for its
//! status: "Die Fixtures müssen gar nicht jedes mal gebaut werden. Die sollten
//! einfach nur exisitieren und dann als Download angeboten werden, wie bei
//! unserem Clienten." So the package writes the file once, and what is held
//! here is the promise the page makes about it: the file it lists is the one
//! the server hands out, by that name and that size, and a server with no file
//! offers none rather than making one.

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

/// One test, because where the files are is the process's environment and two
/// tests setting it at once would read each other's.
#[tokio::test]
async fn the_status_page_offers_the_fixture_zip_the_package_carries_and_nothing_else() {
    let app = router(AppState::new(
        Database::open_in_memory().expect("opens"),
        "0.0.1-test",
    ));

    // **A server with no file offers none**, and writes none: a server run from
    // a build, or one whose package somehow lacks it.
    let empty = tempfile::tempdir().expect("a directory");
    std::env::set_var(ovs_core::FIXTURES_DIR_ENV, empty.path());
    let (status, _, body) = fetch(&app, "/status.json").await;
    assert_eq!(status, StatusCode::OK);
    let page: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(page["fixtures"], serde_json::json!([]));
    assert_eq!(
        std::fs::read_dir(empty.path()).expect("lists").count(),
        0,
        "the server wrote something"
    );

    // **The file the package carries**, written as `ovs-fixtures-zip` writes it.
    let carried = tempfile::tempdir().expect("a directory");
    let name = ovs_proto::desks::archive_name("0.0.1-test");
    let zip = ovs_proto::desks::archive().expect("written");
    std::fs::write(carried.path().join(&name), &zip).expect("writes");
    std::env::set_var(ovs_core::FIXTURES_DIR_ENV, carried.path());

    let (_, _, body) = fetch(&app, "/status.json").await;
    let page: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        page["fixtures"],
        serde_json::json!([{ "name": name, "bytes": zip.len() }])
    );
    assert_eq!(
        page["fixture_desks"],
        serde_json::json!(["grandMA1", "grandMA2", "dot2", "grandMA3"])
    );

    // No session, as the client download has none.
    let (status, headers, served) = fetch(&app, &format!("/fixtures/{name}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/zip");
    assert_eq!(
        headers["content-disposition"],
        format!("attachment; filename=\"{name}\"").as_str()
    );
    assert_eq!(served, zip, "not the file the package carries");

    // And every desk, each in a folder of its own.
    let inside: Vec<String> = ovs_proto::zip::read_stored(&served)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for expected in [
        "grandMA1/TESTBILD MEDIA@OVS MASTER.TXT",
        "grandMA2/testbild_media@ovs_layer@standard.xml",
        "dot2/testbild_media@ovs_layer@standard.xml",
        "grandMA3/testbild media@OVS Layer.gdtf",
    ] {
        assert!(inside.iter().any(|name| name == expected), "{expected}");
    }

    // What is on the disc, and not what the server could have made: the
    // archive is the same bytes either way, so only a file changed under it
    // tells the two apart.
    std::fs::write(carried.path().join(&name), b"PK\x03\x04 changed").expect("writes");
    let (_, _, served) = fetch(&app, &format!("/fixtures/{name}")).await;
    assert_eq!(served, b"PK\x03\x04 changed");

    // Only from that directory, and only what is in it.
    let (status, _, _) = fetch(&app, "/fixtures/..%2F..%2Fetc%2Fshadow").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fetch(&app, "/fixtures/OpenVideoServer-9.9.9-fixtures.zip").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The address the page used to offer, which wrote the archive itself.
    let (status, _, _) = fetch(&app, "/fixtures.zip").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
