//! The compare-and-swap rename the background title generator relies on.

use shuvarie_db::Store;

#[tokio::test]
async fn set_title_if_swaps_while_the_title_is_the_expected_one() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("hello world", None, None, None)
        .await
        .unwrap();

    // A mismatched expectation leaves the stored title untouched.
    let swapped = store
        .set_title_if(id, "not the title", "Mocked title")
        .await
        .unwrap();
    assert!(!swapped);
    assert_eq!(store.load_session(id).await.unwrap().title, "hello world");

    // The provisional title from `create_session` upgrades to the generated
    // one.
    let swapped = store
        .set_title_if(id, "hello world", "Mocked title")
        .await
        .unwrap();
    assert!(swapped);
    assert_eq!(store.load_session(id).await.unwrap().title, "Mocked title");

    // And a second swap on the new title needs its new value as the
    // expectation: the old one no longer matches.
    let swapped = store
        .set_title_if(id, "hello world", "Stale")
        .await
        .unwrap();
    assert!(!swapped);
    assert_eq!(store.load_session(id).await.unwrap().title, "Mocked title");
}

#[tokio::test]
async fn set_title_overwrites_the_title_unconditionally() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("hello world", None, None, None)
        .await
        .unwrap();
    store.set_title(id, "Manual rename").await.unwrap();
    assert_eq!(store.load_session(id).await.unwrap().title, "Manual rename");
}
