use std::{sync::Arc, time::Duration};

use actix_web::web;
use futures::StreamExt;

use jmap::types::jmap::JMAPId;
use jmap_client::{client::Client, event_source::Changes, mailbox::Role, TypeState};
use store::{ahash::AHashSet, RecipientType, Store};
use tokio::sync::mpsc;

use crate::{
    tests::{jmap_mail::ingest_message, store::utils::StoreCompareWith},
    JMAPServer,
};

pub async fn test<T>(server: web::Data<JMAPServer<T>>, client: &mut Client)
where
    T: for<'x> Store<'x> + 'static,
{
    println!("Running EventSource tests...");

    // Create a domain name and a test account
    server.store.recipients.insert(
        "jdoe@example.com".to_string(),
        Arc::new(RecipientType::Individual(1)),
    );

    let mut changes = client
        .event_source(None::<Vec<_>>, false, 1.into(), None)
        .await
        .unwrap();

    let (event_tx, mut event_rx) = mpsc::channel::<Changes>(100);

    tokio::spawn(async move {
        while let Some(change) = changes.next().await {
            if let Err(_err) = event_tx.send(change.unwrap()).await {
                //println!("Error sending event: {}", _err);
                break;
            }
        }
    });

    // Create mailbox and expect state change
    let mailbox_id = client
        .set_default_account_id(JMAPId::new(1).to_string())
        .mailbox_create("EventSource Test", None::<String>, Role::None)
        .await
        .unwrap()
        .take_id();
    assert_state(&mut event_rx, &[TypeState::Mailbox]).await;

    // Multiple changes should be grouped and delivered in intervals
    for num in 0..5 {
        client
            .mailbox_update_sort_order(&mailbox_id, num)
            .await
            .unwrap();
    }
    assert_state(&mut event_rx, &[TypeState::Mailbox]).await;
    assert_ping(&mut event_rx).await; // Pings are only received in cfg(test)

    // Ingest email and expect state change
    ingest_message(
        concat!(
            "From: bill@example.com\r\n",
            "To: jdoe@example.com\r\n",
            "Subject: TPS Report\r\n",
            "\r\n",
            "I'm going to need those TPS reports ASAP. ",
            "So, if you could do that, that'd be great."
        )
        .as_bytes()
        .to_vec(),
        &["jdoe@example.com"],
    )
    .await;

    assert_state(
        &mut event_rx,
        &[
            TypeState::EmailDelivery,
            TypeState::Email,
            TypeState::Thread,
            TypeState::Mailbox,
        ],
    )
    .await;
    assert_ping(&mut event_rx).await;

    // Destroy mailbox
    client.mailbox_destroy(&mailbox_id, true).await.unwrap();

    assert_state(
        &mut event_rx,
        &[TypeState::Email, TypeState::Thread, TypeState::Mailbox],
    )
    .await;
    assert_ping(&mut event_rx).await;
    assert_ping(&mut event_rx).await;

    server.store.assert_is_empty();
}

async fn assert_state(event_rx: &mut mpsc::Receiver<Changes>, state: &[TypeState]) {
    match tokio::time::timeout(Duration::from_millis(700), event_rx.recv()).await {
        Ok(Some(changes)) => {
            assert_eq!(
                changes
                    .changes(&JMAPId::new(1).to_string())
                    .unwrap()
                    .map(|x| x.0)
                    .collect::<AHashSet<&TypeState>>(),
                state.iter().collect::<AHashSet<&TypeState>>()
            );
        }
        result => {
            panic!("Timeout waiting for event {:?}: {:?}", state, result);
        }
    }
}

async fn assert_ping(event_rx: &mut mpsc::Receiver<Changes>) {
    match tokio::time::timeout(Duration::from_millis(1100), event_rx.recv()).await {
        Ok(Some(changes)) => {
            //println!("received {:?}", changes);
            assert!(changes.changes("ping").is_some(),);
        }
        _ => {
            panic!("Did not receive ping.");
        }
    }
}
