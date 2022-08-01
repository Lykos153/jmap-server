use std::{fs, path::PathBuf};

use actix_web::web;
use jmap::types::jmap::JMAPId;
use jmap_client::{client::Client, core::query, email::query::Filter, mailbox::Role};
use store::{ahash::AHashMap, Store};

use crate::{tests::store::utils::StoreCompareWith, JMAPServer};

pub async fn test<T>(server: web::Data<JMAPServer<T>>, client: &mut Client)
where
    T: for<'x> Store<'x> + 'static,
{
    println!("Running SearchSnippet tests...");

    let mailbox_id = client
        .set_default_account_id(JMAPId::new(1).to_string())
        .mailbox_create("JMAP SearchSnippet", None::<String>, Role::None)
        .await
        .unwrap()
        .take_id();

    let mut email_ids = AHashMap::default();

    let mut test_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    test_dir.push("src");
    test_dir.push("tests");
    test_dir.push("resources");
    test_dir.push("jmap_mail_snippet");

    // Import test messages
    for email_name in ["html", "subpart", "text_plain", "text_plain_chinese"] {
        let mut file_name = test_dir.clone();
        file_name.push(format!("{}.eml", email_name));
        let email_id = client
            .email_import(
                fs::read(&file_name).unwrap(),
                [&mailbox_id],
                None::<Vec<&str>>,
                None,
            )
            .await
            .unwrap()
            .take_id();
        email_ids.insert(email_name, email_id);
    }

    // Run tests
    for (filter, email_name, snippet_subject, snippet_preview) in [
        (
            query::Filter::or(vec![
                query::Filter::or(vec![Filter::subject("friend"), Filter::subject("help")]),
                query::Filter::or(vec![Filter::body("secret"), Filter::body("call")]),
            ]),
            "text_plain",
            Some("<mark>Help</mark> a <mark>friend</mark> from Abidjan Côte d'Ivoire"),
            Some(concat!(
                "d'Ivoire. He <mark>secretly</mark> <mark>called</mark> me on his bedside ",
                "and told me that he has a sum of $7.5M (Seven Million five Hundred Thousand",
                " Dollars) left in a suspense account in a local bank here in Abidjan Côte ",
                "d'Ivoire, that he used my name a")),
        ),
        (
            Filter::text("côte").into(),
            "text_plain",
            Some("Help a friend from Abidjan <mark>Côte</mark> d'Ivoire"),
            Some(concat!(
                "in Abidjan <mark>Côte</mark> d'Ivoire. He secretly called me on ",
                "his bedside and told me that he has a sum of $7.5M (Seven ",
                "Million five Hundred Thousand Dollars) left in a suspense ",
                "account in a local bank here in Abidjan <mark>Côte</mark> d'Ivoire, that "
            )),
        ),
        (
            Filter::text("\"your country\"").into(),
            "text_plain",
            None,
            Some(concat!(
                "over to <mark>your</mark> <mark>country</mark> to further my education and ",
                "to secure a residential permit for me in <mark>your</mark> <mark>country",
                "</mark>. Moreover, I am willing to offer you 30 percent of the total sum as ",
                "compensation for your effort inp",
            )),
        ),
        (
            Filter::text("overseas").into(),
            "text_plain",
            None,
            Some("nominated account <mark>overseas</mark>. "),
        ),
        (
            Filter::text("孫子兵法").into(),
            "text_plain_chinese",
            Some("<mark>孫子兵法</mark>"),
            Some(concat!(
                "&lt;&quot;<mark>孫子兵法</mark>：&quot;&gt; 孫子曰：兵者，國之大事，死生之地，",
                "存亡之道，不可不察也。 孫子曰：凡用兵之法，馳車千駟，革車千乘，帶甲十萬；千里饋糧，",
                "則內外之費賓客之用，")),
        ),
        (
            Filter::text("cia").into(),
            "subpart",
            None,
            Some("shouldn't the <mark>CIA</mark> have something like that? Bill"),
        ),
        (
            Filter::text("frösche").into(),
            "html",
            Some("Die Hasen und die <mark>Frösche</mark>"),
            Some(concat!(
            "und die <mark>Frösche</mark> Die Hasen klagten einst über ihre mißliche Lage; ",
            "&quot;wir leben&quot;, sprach ein Redner, &quot;in steter Furcht vor Menschen und ",
            "Tieren, eine Beute der Hunde, der Adler, ja fast aller Raubtiere! ",
            "Unsere stete Angst ist är")),
        ),
    ] {
        let mut request = client.build();
        let result_ref = request
            .query_email()
            .filter(filter.clone())
            .result_reference();
        request
            .get_search_snippet()
            .filter(filter)
            .email_ids_ref(result_ref);
        let response = request
            .send()
            .await
            .unwrap()
            .unwrap_method_responses()
            .pop()
            .unwrap()
            .unwrap_get_search_snippet()
            .unwrap();
        let snippet = response
            .snippet(email_ids.get(email_name).unwrap())
            .unwrap();
        assert_eq!(snippet_subject, snippet.subject());
        assert_eq!(snippet_preview, snippet.preview());
        assert!(
            snippet.preview().map_or(0, |p| p.len()) <= 255,
            "len: {}",
            snippet.preview().map_or(0, |p| p.len())
        );
    }

    // Destroy test data
    client.mailbox_destroy(&mailbox_id, true).await.unwrap();

    server.store.assert_is_empty();
}
