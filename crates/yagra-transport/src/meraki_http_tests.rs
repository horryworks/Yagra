// SPDX-License-Identifier: AGPL-3.0-only
//! The Meraki transport's HTTP half, run for real against a scripted Dashboard on a local socket
//! (ADR-166).
//!
//! Before this, every Meraki test stopped above HTTP: the parsers and `next_page` were tested as
//! pure functions, core faked `MerakiDirectory`, the poller faked `Transport`. The round trip itself
//! — the `Link` walk, the 429 retry, the status mapping, the allow-list on a next link — had never
//! been executed by anything but a deployment. It could not be: the allow-list admits only https
//! Meraki hosts, so no test could point a session at a socket it controls. A [`MerakiWireOrigin`]
//! changes where requests are physically sent without changing what the allow-list sees, and that
//! is what lets these run.
//!
//! The fake is the third copy of the same small shape (`yagra-core`'s `rca/testsupport.rs` and the
//! one in `bigquery.rs` are the others). It is not shared because `yagra-core` is a binary crate and
//! `yagra-common` carries no tokio; this one adds response headers, which the other two do not need.
//!
//! Every value here is invented (ADR-165): serials, network ids and addresses are made up, and the
//! addresses come from the documentation range.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::meraki::collect;
use crate::{
    fetch_inventory, list_organizations, MerakiAvailability, MerakiCollectSpec, MerakiFetchError,
    MerakiTier, MerakiWireOrigin,
};
use yagra_common::MerakiListing;

/// The stored base URL every test uses — the real one, because the allow-list admits nothing else.
const BASE: &str = "https://api.meraki.com";
const KEY: &str = "test-key";
const TIMEOUT: Duration = Duration::from_secs(5);

/// One scripted answer.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.to_owned(),
        }
    }

    fn ok(body: &str) -> Self {
        Self::json(200, body)
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// A `Link` header in the shape the Dashboard sends: several relations, `next` among them.
    fn next(self, url: &str) -> Self {
        self.header(
            "Link",
            &format!("<{BASE}/api/v1/organizations/1/first>; rel=first, <{url}>; rel=next"),
        )
    }
}

/// What the fake was asked: each request's line and headers, verbatim, in arrival order.
type Seen = Arc<Mutex<Vec<String>>>;

/// Start a fake Dashboard answering from `replies` in order. A request beyond the script is
/// answered 500, so a test that sends one more request than it expected sees it in its assertions.
async fn serve(replies: Vec<Reply>) -> (MerakiWireOrigin, SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let queue = Arc::new(Mutex::new(replies));
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            let queue = queue.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // A GET has no body: the head is everything up to the blank line.
                while !String::from_utf8_lossy(&buf).contains("\r\n\r\n") {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let head = text.split("\r\n\r\n").next().unwrap_or_default().to_owned();
                log.lock().unwrap().push(head);
                let reply = {
                    let mut q = queue.lock().unwrap();
                    if q.is_empty() {
                        Reply::json(500, "\"unscripted request\"")
                    } else {
                        q.remove(0)
                    }
                };
                let extra: String = reply
                    .headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .collect();
                let res = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body
                );
                let _ = sock.write_all(res.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    let origin = MerakiWireOrigin::parse(&format!("http://{addr}")).unwrap();
    (origin, addr, seen)
}

/// Each request's first line (`GET /path?query HTTP/1.1`).
fn lines(seen: &Seen) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|h| h.lines().next().unwrap_or_default().to_owned())
        .collect()
}

/// The value of header `name` on request `i`, case-insensitively.
fn header(seen: &Seen, i: usize, name: &str) -> Option<String> {
    let log = seen.lock().unwrap();
    let prefix = format!("{}:", name.to_ascii_lowercase());
    log.get(i)?
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim().to_owned())
}

fn spec(tier: MerakiTier, networks: &[&str]) -> MerakiCollectSpec {
    MerakiCollectSpec {
        org_id: "1".to_owned(),
        base_url: BASE.to_owned(),
        api_key: KEY.to_owned(),
        tier,
        network_ids: networks.iter().map(|n| (*n).to_owned()).collect(),
        per_page: 1000,
        target_rps: 1000.0,
        interval_secs: 1_800,
        port_names: false,
        ssid_statuses: false,
    }
}

/// The logical URL of the availability collect's first page. It names no network, whichever are
/// watched: a collect asks the whole organization and keeps the watched rows (ADR-164 決定 22).
const AVAILABILITY: &str =
    "https://api.meraki.com/api/v1/organizations/1/devices/availabilities?perPage=1000";
const AVAILABILITY_LINE: &str =
    "GET /api/v1/organizations/1/devices/availabilities?perPage=1000 HTTP/1.1";

/// An availability row in the Dashboard's shape — the network is an object, `network.id`.
fn device_up_in(serial: &str, network: &str) -> String {
    format!(r#"{{"serial":"{serial}","status":"online","network":{{"id":"{network}"}}}}"#)
}

/// An online device in network `N_1`, the network most tests watch.
fn device_up(serial: &str) -> String {
    device_up_in(serial, "N_1")
}

#[tokio::test]
async fn every_page_of_an_inventory_is_fetched_through_the_wire_origin() {
    let page2 = format!("{BASE}/api/v1/organizations/1/networks?perPage=1000&startingAfter=N_1");
    let page3 = format!("{BASE}/api/v1/organizations/1/networks?perPage=1000&startingAfter=N_2");
    let (origin, addr, seen) = serve(vec![
        Reply::ok(r#"[{"id":"N_1","name":"site-a"}]"#).next(&page2),
        Reply::ok(r#"[{"id":"N_2","name":"site-b"}]"#).next(&page3),
        Reply::ok(r#"[{"id":"N_3","name":"site-c"}]"#),
        Reply::ok(
            r#"[{"serial":"Q2XX-TEST-0001","name":"ap-01","model":"MR46","productType":"wireless","networkId":"N_1","lanIp":"192.0.2.10"}]"#,
        ),
        Reply::ok(&format!("[{}]", device_up("Q2XX-TEST-0001"))),
    ])
    .await;

    let inv = fetch_inventory(BASE, KEY, "1", 1000.0, TIMEOUT, Some(&origin))
        .await
        .expect("a complete inventory");

    assert_eq!(
        inv.networks.len(),
        3,
        "one network per page, all three pages"
    );
    assert_eq!(inv.devices.len(), 1);
    assert_eq!(inv.devices[0].info.lan_ip.as_deref(), Some("192.0.2.10"));
    assert_eq!(
        inv.devices[0].availability,
        Some(MerakiAvailability::Online)
    );
    assert_eq!(
        lines(&seen),
        vec![
            "GET /api/v1/organizations/1/networks?perPage=1000 HTTP/1.1",
            "GET /api/v1/organizations/1/networks?perPage=1000&startingAfter=N_1 HTTP/1.1",
            "GET /api/v1/organizations/1/networks?perPage=1000&startingAfter=N_2 HTTP/1.1",
            "GET /api/v1/organizations/1/devices?perPage=1000 HTTP/1.1",
            "GET /api/v1/organizations/1/devices/availabilities?perPage=1000 HTTP/1.1",
        ]
    );
    // Physically sent to the fake, and carrying the key the way the Dashboard expects it.
    assert_eq!(header(&seen, 0, "host"), Some(addr.to_string()));
    assert_eq!(
        header(&seen, 4, "authorization"),
        Some(format!("Bearer {KEY}"))
    );
}

#[tokio::test]
async fn listing_organizations_goes_through_the_wire_origin() {
    let (origin, _, seen) = serve(vec![Reply::ok(
        r#"[{"id":"100","name":"org-a"},{"id":200,"name":"org-b"}]"#,
    )])
    .await;

    let orgs = list_organizations(BASE, KEY, TIMEOUT, Some(&origin))
        .await
        .expect("the organizations");

    let ids: Vec<&str> = orgs.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["100", "200"], "a numeric id is read as text");
    assert_eq!(
        lines(&seen),
        vec!["GET /api/v1/organizations?perPage=1000 HTTP/1.1"]
    );
}

#[tokio::test]
async fn a_next_link_to_a_regional_shard_is_followed_and_sent_to_the_wire_origin() {
    let shard = "https://n123.meraki.com/api/v1/organizations/1/devices/availabilities?perPage=1000&startingAfter=Q2XX-TEST-0001";
    let (origin, addr, seen) = serve(vec![
        Reply::ok(&format!("[{}]", device_up("Q2XX-TEST-0001"))).next(shard),
        Reply::ok(&format!("[{}]", device_up("Q2XX-TEST-0002"))),
    ])
    .await;

    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a collect");

    assert_eq!(got.stopped, None);
    assert_eq!(got.observations.len(), 2);
    let seen_lines = lines(&seen);
    assert_eq!(seen_lines.len(), 2);
    assert_eq!(
        seen_lines[1],
        "GET /api/v1/organizations/1/devices/availabilities?perPage=1000&startingAfter=Q2XX-TEST-0001 HTTP/1.1"
    );
    // The shard host was checked and then replaced like any other.
    assert_eq!(header(&seen, 1, "host"), Some(addr.to_string()));
}

#[tokio::test]
async fn a_next_link_off_the_allow_list_is_not_followed() {
    let (origin, _, seen) = serve(vec![Reply::ok(&format!(
        "[{}]",
        device_up("Q2XX-TEST-0001")
    ))
    .next("https://evil.example/api/v1/organizations/1/devices/availabilities")])
    .await;

    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await;

    assert_eq!(got, Err(MerakiFetchError::Host));
    assert_eq!(
        lines(&seen).len(),
        1,
        "the key is never sent to the off-list host"
    );
}

/// 🚨 **This is the test that pins `visited` to logical URLs.** Compare pages by the URL actually
/// sent and a `Link` naming the page just fetched never matches, so the walk follows it to the
/// fifty-page cap — the fake then answers the second request `500` and this assertion names it.
#[tokio::test]
async fn a_next_link_naming_the_page_just_fetched_stops_as_a_cycle() {
    let (origin, _, seen) = serve(vec![Reply::ok(&format!(
        "[{}]",
        device_up("Q2XX-TEST-0001")
    ))
    .next(AVAILABILITY)])
    .await;

    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a partial collect is still an answer");

    assert_eq!(got.stopped, Some(MerakiFetchError::Truncated));
    assert_eq!(got.observations.len(), 1, "page one is kept");
    assert_eq!(lines(&seen), vec![AVAILABILITY_LINE]);
}

#[tokio::test]
async fn a_429_is_retried_on_the_same_request_and_then_answered() {
    let (origin, _, seen) = serve(vec![
        Reply::json(429, r#"{"errors":["rate limited"]}"#).header("Retry-After", "0"),
        Reply::ok(&format!("[{}]", device_up("Q2XX-TEST-0001"))),
    ])
    .await;

    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a collect");

    assert_eq!(got.stopped, None);
    assert_eq!(got.observations.len(), 1);
    assert_eq!(lines(&seen), vec![AVAILABILITY_LINE, AVAILABILITY_LINE]);
}

#[tokio::test]
async fn seven_429s_in_a_row_end_the_listing_as_rate_limited() {
    let replies = (0..7)
        .map(|_| Reply::json(429, "[]").header("Retry-After", "0"))
        .collect();
    let (origin, _, seen) = serve(replies).await;

    let got = fetch_inventory(BASE, KEY, "1", 1000.0, TIMEOUT, Some(&origin)).await;

    assert_eq!(got, Err(MerakiFetchError::RateLimited));
    assert_eq!(lines(&seen).len(), 7, "the first try and six retries");
}

#[tokio::test]
async fn a_200_that_is_not_an_array_is_malformed_for_both_readers() {
    let body = r#"{"errors":["not a listing"]}"#;
    let (origin, _, _) = serve(vec![Reply::ok(body)]).await;
    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await;
    assert_eq!(got, Err(MerakiFetchError::Malformed));

    let (origin, _, _) = serve(vec![Reply::ok(body)]).await;
    let got = fetch_inventory(BASE, KEY, "1", 1000.0, TIMEOUT, Some(&origin)).await;
    assert_eq!(got, Err(MerakiFetchError::Malformed));
}

#[tokio::test]
async fn a_refused_key_is_an_auth_failure() {
    let (origin, _, _) = serve(vec![Reply::json(401, r#"{"errors":["Invalid API key"]}"#)]).await;
    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await;
    assert_eq!(got, Err(MerakiFetchError::Auth(401)));

    let (origin, _, _) = serve(vec![Reply::json(403, r#"{"errors":["forbidden"]}"#)]).await;
    let got = fetch_inventory(BASE, KEY, "1", 1000.0, TIMEOUT, Some(&origin)).await;
    assert_eq!(got, Err(MerakiFetchError::Auth(403)));
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (origin, _, seen) = serve(vec![
        Reply::json(302, "[]").header("Location", "/api/v1/organizations/1/elsewhere")
    ])
    .await;

    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a status the lenient reader survives");

    assert_eq!(got.stopped, Some(MerakiFetchError::Status(302)));
    assert_eq!(lines(&seen).len(), 1, "the redirect target is never asked");
}

#[tokio::test]
async fn a_500_on_page_two_keeps_page_one_for_a_collect_and_fails_the_inventory() {
    let page2 = format!("{AVAILABILITY}&startingAfter=Q2XX-TEST-0001");
    let (origin, _, _) = serve(vec![
        Reply::ok(&format!("[{}]", device_up("Q2XX-TEST-0001"))).next(&page2),
        Reply::json(500, r#"{"errors":["internal"]}"#),
    ])
    .await;
    let got = collect(
        &spec(MerakiTier::Availability, &["N_1"]),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a partial collect is still an answer");
    assert_eq!(got.observations.len(), 1, "page one is kept");
    assert_eq!(got.stopped, Some(MerakiFetchError::Status(500)));
    assert_eq!(got.failure(), None, "the Dashboard did answer");

    let networks2 =
        format!("{BASE}/api/v1/organizations/1/networks?perPage=1000&startingAfter=N_1");
    let (origin, _, _) = serve(vec![
        Reply::ok(r#"[{"id":"N_1","name":"site-a"}]"#).next(&networks2),
        Reply::json(500, r#"{"errors":["internal"]}"#),
    ])
    .await;
    let got = fetch_inventory(BASE, KEY, "1", 1000.0, TIMEOUT, Some(&origin)).await;
    assert_eq!(got, Err(MerakiFetchError::Status(500)));
}

#[tokio::test]
async fn a_wire_origin_does_not_admit_a_base_url_the_allow_list_refuses() {
    let (origin, _, seen) = serve(Vec::new()).await;
    for base in ["https://evil.example", "http://api.meraki.com"] {
        let mut s = spec(MerakiTier::Availability, &["N_1"]);
        s.base_url = base.to_owned();
        assert_eq!(
            collect(&s, TIMEOUT, Some(&origin)).await,
            Err(MerakiFetchError::Config),
            "{base}"
        );
        assert_eq!(
            fetch_inventory(base, KEY, "1", 1000.0, TIMEOUT, Some(&origin)).await,
            Err(MerakiFetchError::Config),
            "{base}"
        );
        assert!(
            list_organizations(base, KEY, TIMEOUT, Some(&origin))
                .await
                .is_err(),
            "{base}"
        );
    }
    assert!(lines(&seen).is_empty(), "nothing was sent anywhere");
}

/// The serials a collect observed, in its (sorted) order.
fn serials(got: &crate::MerakiCollected) -> Vec<&str> {
    got.observations.iter().map(|o| o.serial.as_str()).collect()
}

/// Three devices in three networks, and one whose row names no network at all.
fn four_devices() -> String {
    format!(
        "[{},{},{},{}]",
        device_up_in("Q2XX-TEST-0001", "N_1"),
        device_up_in("Q2XX-TEST-0002", "N_2"),
        device_up_in("Q2XX-TEST-0009", "N_9"),
        r#"{"serial":"Q2XX-TEST-0010","status":"online"}"#,
    )
}

#[tokio::test]
async fn a_collect_asks_the_whole_organization_and_keeps_only_the_watched_networks() {
    let (origin, _, seen) = serve(vec![Reply::ok(&four_devices())]).await;

    let got = collect(
        &spec(MerakiTier::Availability, &["N_1", "N_2"]),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a collect");

    assert_eq!(
        serials(&got),
        vec!["Q2XX-TEST-0001", "Q2XX-TEST-0002"],
        "an unwatched network's device and one naming no network are dropped"
    );
    assert_eq!(
        lines(&seen),
        vec![AVAILABILITY_LINE],
        "no network in the URL"
    );
}

/// On the bus an empty list means every network (ADR-164 決定 16). Core no longer sends one, but a
/// message omitting the field still decodes to it, so it must keep meaning what it always meant.
#[tokio::test]
async fn an_empty_network_list_keeps_every_row() {
    let (origin, _, seen) = serve(vec![Reply::ok(&four_devices())]).await;

    let got = collect(&spec(MerakiTier::Availability, &[]), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");

    assert_eq!(
        serials(&got),
        vec![
            "Q2XX-TEST-0001",
            "Q2XX-TEST-0002",
            "Q2XX-TEST-0009",
            "Q2XX-TEST-0010"
        ]
    );
    assert_eq!(lines(&seen), vec![AVAILABILITY_LINE]);
}

/// 🚨 **The 414 this replaced.** The Dashboard's nginx refuses a request target past 8,177
/// characters (measured 2026-09-22); 434 network ids of the real length made the old query 16,662
/// characters, so every collect of that organization failed. The request must not grow with the
/// number of watched networks at all.
#[tokio::test]
async fn hundreds_of_watched_networks_do_not_lengthen_the_request() {
    let many: Vec<String> = (0..500).map(|i| format!("L_{i:018}")).collect();
    let watched: Vec<&str> = many.iter().map(String::as_str).collect();
    let (origin, _, seen) = serve(vec![Reply::ok(&format!(
        "[{}]",
        device_up_in("Q2XX-TEST-0001", &many[499])
    ))])
    .await;

    let got = collect(
        &spec(MerakiTier::Availability, &watched),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a collect");

    assert_eq!(serials(&got), vec!["Q2XX-TEST-0001"]);
    assert_eq!(lines(&seen), vec![AVAILABILITY_LINE]);
}

#[tokio::test]
async fn the_uplink_tier_asks_the_whole_organization_and_keeps_the_watched_networks() {
    // Both uplink listings name the network as `networkId`, not `network.id`.
    let loss = r#"[
        {"networkId":"N_1","serial":"Q2XX-TEST-0001","uplink":"wan1","ip":"192.0.2.1",
         "timeSeries":[{"ts":"2026-09-22T00:00:00Z","lossPercent":0.0,"latencyMs":12.5}]},
        {"networkId":"N_9","serial":"Q2XX-TEST-0009","uplink":"wan1","ip":"192.0.2.9",
         "timeSeries":[{"ts":"2026-09-22T00:00:00Z","lossPercent":1.0,"latencyMs":30.0}]}]"#;
    let statuses = r#"[
        {"networkId":"N_2","serial":"Q2XX-TEST-0002","uplinks":[{"interface":"wan1","status":"active"}]},
        {"networkId":"N_9","serial":"Q2XX-TEST-0009","uplinks":[{"interface":"wan1","status":"active"}]}]"#;
    let (origin, _, seen) =
        serve(vec![Reply::ok(loss), Reply::ok(statuses), Reply::ok("[]")]).await;

    let got = collect(
        &spec(MerakiTier::Uplink, &["N_1", "N_2"]),
        TIMEOUT,
        Some(&origin),
    )
    .await
    .expect("a collect");

    assert_eq!(got.stopped, None);
    assert_eq!(serials(&got), vec!["Q2XX-TEST-0001", "Q2XX-TEST-0002"]);
    assert_eq!(
        lines(&seen),
        vec![
            "GET /api/v1/organizations/1/devices/uplinksLossAndLatency?timespan=300&perPage=1000 HTTP/1.1",
            "GET /api/v1/organizations/1/appliance/uplink/statuses?perPage=1000 HTTP/1.1",
            // Auto VPN, last, at the largest page it accepts (ADR-164 決定 25).
            "GET /api/v1/organizations/1/appliance/vpn/statuses?perPage=300 HTTP/1.1",
        ]
    );
}

/// ADR-164 決定 25: the uplink tier's third read is Auto VPN, at `perPage=300` — the real Dashboard
/// answers `perPage=1000` with 400 — and it follows the listing's pages like any other.
#[tokio::test]
async fn the_uplink_tier_reads_auto_vpn_last_at_the_page_size_it_accepts_and_pages_through_it() {
    let page2 = format!(
        "{BASE}/api/v1/organizations/1/appliance/vpn/statuses?perPage=300&startingAfter=N_1"
    );
    let vpn1 = r#"[
        {"networkId":"N_1","deviceSerial":"Q2XX-TEST-0001","deviceStatus":"online","vpnMode":"spoke",
         "merakiVpnPeers":[{"networkId":"N_H","reachability":"reachable"},
                           {"networkId":"N_G","reachability":"unreachable"}]}]"#;
    let vpn2 = r#"[
        {"networkId":"N_H","deviceSerial":"Q2XX-TEST-0008","deviceStatus":"online","vpnMode":"hub",
         "merakiVpnPeers":[{"networkId":"N_1","reachability":"reachable"}]},
        {"networkId":"N_G","deviceSerial":"Q2XX-TEST-0009","deviceStatus":"online","vpnMode":"hub",
         "merakiVpnPeers":[]}]"#;
    let (origin, _, seen) = serve(vec![
        Reply::ok("[]"),
        Reply::ok("[]"),
        Reply::ok(vpn1).next(&page2),
        Reply::ok(vpn2),
    ])
    .await;

    let got = collect(&spec(MerakiTier::Uplink, &["N_1"]), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");

    assert_eq!(got.failure(), None, "{got:?}");
    assert_eq!(serials(&got), vec!["Q2XX-TEST-0001"], "only the watched MX");
    let pct = got.observations[0]
        .samples
        .iter()
        .find(|s| s.metric == "meraki_vpn_hubs_unreachable_pct")
        .map(|s| s.value);
    assert_eq!(
        pct,
        Some(50.0),
        "one of its two hubs, judged by rows it does not watch"
    );
    let sent = lines(&seen);
    assert_eq!(sent.len(), 4, "{sent:?}");
    assert_eq!(
        sent[2],
        "GET /api/v1/organizations/1/appliance/vpn/statuses?perPage=300 HTTP/1.1"
    );
    assert!(
        sent[3].starts_with(
            "GET /api/v1/organizations/1/appliance/vpn/statuses?perPage=300&startingAfter=N_1 "
        ),
        "{}",
        sent[3]
    );
}

/// ADR-164 決定 25: an Auto VPN read that fails costs only its own readings. The uplinks' loss and
/// status still arrive, and the collect says which read failed — it used to be hidden whenever
/// another listing of the tier answered.
#[tokio::test]
async fn a_failed_vpn_read_keeps_the_uplinks_readings_and_names_itself() {
    let loss = r#"[{"networkId":"N_1","serial":"Q2XX-TEST-0001","uplink":"wan1",
        "timeSeries":[{"ts":"2026-09-22T00:00:00Z","lossPercent":0.0,"latencyMs":12.5}]}]"#;
    let statuses = r#"[{"networkId":"N_1","serial":"Q2XX-TEST-0001",
        "uplinks":[{"interface":"wan1","status":"active"}]}]"#;
    for (reply, why) in [
        (
            Reply::json(400, r#"{"errors":["mock"]}"#),
            MerakiFetchError::Status(400),
        ),
        (
            Reply::ok(r#"{"not":"a list"}"#),
            MerakiFetchError::Malformed,
        ),
        (
            Reply::json(403, r#"{"errors":["forbidden"]}"#),
            MerakiFetchError::Auth(403),
        ),
    ] {
        let (origin, _, seen) = serve(vec![Reply::ok(loss), Reply::ok(statuses), reply]).await;
        let got = collect(&spec(MerakiTier::Uplink, &["N_1"]), TIMEOUT, Some(&origin))
            .await
            .expect("the collect still answers");
        assert_eq!(
            got.failed_listing,
            Some((MerakiListing::ApplianceVpnStatuses, why)),
            "{why:?}"
        );
        assert_eq!(got.failure(), Some(why));
        let metrics: Vec<&str> = got.observations[0]
            .samples
            .iter()
            .map(|s| s.metric.as_str())
            .collect();
        assert!(metrics.contains(&"meraki_uplink_loss_pct"), "{metrics:?}");
        assert!(metrics.contains(&"meraki_uplink_status"), "{metrics:?}");
        assert!(
            !metrics.iter().any(|m| m.starts_with("meraki_vpn_")),
            "{metrics:?}"
        );
        assert_eq!(lines(&seen).len(), 3);
    }
}

/// The first read of a tier is not contained: a refused key there is the whole collect refused, as
/// before — one request, and nothing after it.
#[tokio::test]
async fn a_refused_key_on_the_first_uplink_read_still_refuses_the_whole_collect() {
    let (origin, _, seen) =
        serve(vec![Reply::json(401, r#"{"errors":["Invalid API key"]}"#)]).await;
    let got = collect(&spec(MerakiTier::Uplink, &["N_1"]), TIMEOUT, Some(&origin)).await;
    assert_eq!(got, Err(MerakiFetchError::Auth(401)));
    assert_eq!(lines(&seen).len(), 1);
}

/// The traffic tier asks every MX's uplink usage over the tier's interval, with no page size —
/// the listing documents none — and keeps the watched networks' rows (ADR-164 決定 23). What it
/// replaced asked `summary/top/devices/byUsage` for an hour, which the real Dashboard refuses.
#[tokio::test]
async fn the_traffic_tier_asks_uplink_usage_over_its_interval_and_keeps_the_watched_rows() {
    let rows = r#"[
        {"networkId":"N_1","name":"site-a","byUplink":[
          {"serial":"Q2XX-TEST-0001","interface":"wan1","sent":1125000,"received":2250000}]},
        {"networkId":"N_9","name":"site-z","byUplink":[
          {"serial":"Q2XX-TEST-0009","interface":"wan1","sent":99,"received":99}]}]"#;
    let (origin, _, seen) = serve(vec![Reply::ok(rows)]).await;

    let got = collect(&spec(MerakiTier::Traffic, &["N_1"]), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");

    assert_eq!(serials(&got), vec!["Q2XX-TEST-0001"]);
    let sent_bps = got.observations[0]
        .samples
        .iter()
        .find(|s| s.metric == "meraki_uplink_sent_bps")
        .map(|s| (s.ifindex, s.value));
    assert_eq!(
        sent_bps,
        Some((Some(1), 5_000.0)),
        "1,125,000 bytes over 1,800 s"
    );
    assert_eq!(
        lines(&seen),
        vec![
            "GET /api/v1/organizations/1/appliance/uplinks/usage/byNetwork?timespan=1800 HTTP/1.1"
        ]
    );
}

/// 🚨 A 200 that is not a list is not an answer (決定 19) — on the traffic listing too.
#[tokio::test]
async fn a_traffic_answer_that_is_not_a_list_fails_the_collect() {
    let (origin, _, _) = serve(vec![Reply::ok(r#"{"networkId":"N_1"}"#)]).await;
    let got = collect(&spec(MerakiTier::Traffic, &["N_1"]), TIMEOUT, Some(&origin)).await;
    assert_eq!(got, Err(MerakiFetchError::Malformed));
}

// ── The switch-port tier (ADR-167) ──────────────────────────────────────────────────────────

/// A switch-port status page in the envelope the Dashboard answers with.
fn status_page(rows: &str) -> String {
    format!(r#"{{"items":[{rows}],"meta":{{"counts":{{"items":{{"total":3,"remaining":0}}}}}}}}"#)
}

fn switch_status(serial: &str, network: &str, ports: &str) -> String {
    format!(r#"{{"serial":"{serial}","network":{{"id":"{network}"}},"ports":[{ports}]}}"#)
}

const PORT_1_UP: &str =
    r#"{"portId":"1","enabled":true,"status":"Connected","speed":"1 Gbps","duplex":"full"}"#;
const PORT_2_DOWN: &str =
    r#"{"portId":"2","enabled":true,"status":"Disconnected","speed":"","duplex":""}"#;

fn usage_page(serial: &str, network: &str) -> String {
    format!(
        r#"{{"items":[{{"serial":"{serial}","network":{{"id":"{network}"}},"ports":[
            {{"portId":"1","intervals":[{{"startTs":"2026-09-22T11:40:00.000000Z",
              "endTs":"2026-09-22T11:45:00.000000Z",
              "data":{{"usage":{{"total":1773,"upstream":1183,"downstream":590}}}},
              "bandwidth":{{"usage":{{"total":48.4,"upstream":32.3,"downstream":16.1}}}}}}]}}]}}],
            "meta":{{"counts":{{"items":{{"total":1,"remaining":0}}}}}}}}"#
    )
}

fn switch_ports_spec(port_names: bool) -> MerakiCollectSpec {
    MerakiCollectSpec {
        port_names,
        ..spec(MerakiTier::SwitchPorts, &["N_1"])
    }
}

/// `t0` and `t1` out of the usage request's line.
fn bucket_of(line: &str) -> (u64, u64) {
    let query = line.split_once('?').expect("a query").1;
    let param = |name: &str| -> u64 {
        query
            .split(['&', ' '])
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
            .expect(name)
            .parse()
            .expect("a number")
    };
    (param("t0"), param("t1"))
}

/// The whole tier, through HTTP: the status listing paged at the size it accepts and read out of
/// its envelope, the watched networks kept, one settled usage bucket asked for, the configured
/// names not asked for, and every read organization-wide — no `networkIds[]` (ADR-164 決定 22).
#[tokio::test]
async fn the_switch_port_tier_reads_statuses_then_one_settled_usage_bucket() {
    let page_two = format!(
        "{BASE}/api/v1/organizations/1/switch/ports/statuses/bySwitch?perPage=20&startingAfter=Q2SW-0001"
    );
    let first = status_page(&format!(
        "{},{}",
        switch_status("Q2SW-0001", "N_1", &format!("{PORT_1_UP},{PORT_2_DOWN}")),
        switch_status("Q2SW-0009", "N_9", PORT_1_UP),
    ));
    let second = status_page(&switch_status("Q2SW-0002", "N_1", PORT_1_UP));
    let (origin, _, seen) = serve(vec![
        Reply::ok(&first).next(&page_two),
        Reply::ok(&second),
        Reply::ok(&usage_page("Q2SW-0001", "N_1")),
    ])
    .await;

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let got = collect(&switch_ports_spec(false), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");

    assert_eq!(got.failure(), None);
    assert_eq!(
        serials(&got),
        vec!["Q2SW-0001", "Q2SW-0002"],
        "N_9 is not watched"
    );
    let one = &got.observations[0];
    let reading = |metric: &str, ifindex: u32| {
        one.samples
            .iter()
            .find(|s| s.metric == metric && s.ifindex == Some(ifindex))
            .map(|s| s.value)
    };
    assert_eq!(reading("if_oper_status", 1), Some(1.0));
    assert_eq!(reading("if_oper_status", 2), Some(2.0));
    assert_eq!(reading("if_high_speed", 1), Some(1000.0));
    assert_eq!(reading("meraki_port_in_bps", 1), Some(16_100.0));
    assert_eq!(reading("meraki_port_out_bps", 1), Some(32_300.0));
    assert_eq!(
        one.ports.iter().map(|p| p.ifindex).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(one.ports.iter().all(|p| p.alias.is_none()));
    // The second switch has statuses and no usage: its ports are there, its traffic is not.
    let two = &got.observations[1];
    assert!(two
        .samples
        .iter()
        .all(|s| !s.metric.starts_with("meraki_port_")));
    assert_eq!(two.ports.len(), 1);

    let sent = lines(&seen);
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert_eq!(
        sent[0],
        "GET /api/v1/organizations/1/switch/ports/statuses/bySwitch?perPage=20 HTTP/1.1"
    );
    assert!(sent[1].contains("startingAfter=Q2SW-0001"), "{}", sent[1]);
    assert!(
        sent[2].starts_with(
            "GET /api/v1/organizations/1/switch/ports/usage/history/byDevice/byInterval?interval=300&t0="
        ) && sent[2].ends_with("&perPage=50 HTTP/1.1"),
        "{}",
        sent[2]
    );
    let (t0, t1) = bucket_of(&sent[2]);
    assert_eq!((t1 - t0, t1 % 300), (300, 0));
    assert!(
        before - t1 >= 720,
        "the bucket ended {}s before the collect",
        before - t1
    );
    assert!(sent.iter().all(|l| !l.contains("networkIds")), "{sent:?}");
}

/// 決定 1: the names are read only when core asks, from the listing that is a bare array, and a
/// port with none gets an empty alias rather than keeping an old one.
#[tokio::test]
async fn the_configured_port_names_are_read_only_when_asked_for() {
    let statuses = status_page(&switch_status(
        "Q2SW-0001",
        "N_1",
        &format!("{PORT_1_UP},{PORT_2_DOWN}"),
    ));
    let config = r#"[{"serial":"Q2SW-0001","network":{"id":"N_1"},"ports":[
        {"portId":"1","name":"to core"},{"portId":"2","name":null}]}]"#;
    let (origin, _, seen) = serve(vec![
        Reply::ok(&statuses),
        Reply::ok(&usage_page("Q2SW-0001", "N_1")),
        Reply::ok(config),
    ])
    .await;

    let got = collect(&switch_ports_spec(true), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");

    assert_eq!(got.failure(), None);
    let aliases: Vec<(u32, Option<&str>)> = got.observations[0]
        .ports
        .iter()
        .map(|p| (p.ifindex, p.alias.as_deref()))
        .collect();
    assert_eq!(aliases, [(1, Some("to core")), (2, Some(""))]);
    assert_eq!(
        lines(&seen)[2],
        "GET /api/v1/organizations/1/switch/ports/bySwitch?perPage=50 HTTP/1.1"
    );
}

/// 決定 2, through HTTP. A usage answer in the wrong shape costs the traffic and names itself; the
/// statuses read before it still arrive. The same answer on the statuses read fails the collect —
/// the first read is not contained.
#[tokio::test]
async fn a_switch_port_listing_in_the_wrong_shape_is_malformed() {
    let statuses = status_page(&switch_status("Q2SW-0001", "N_1", PORT_1_UP));
    let (origin, _, _) = serve(vec![
        Reply::ok(&statuses),
        Reply::ok(r#"[{"serial":"Q2SW-0001","ports":[]}]"#),
    ])
    .await;
    let got = collect(&switch_ports_spec(false), TIMEOUT, Some(&origin))
        .await
        .expect("the collect still answers");
    assert_eq!(
        got.failed_listing,
        Some((MerakiListing::SwitchPortUsage, MerakiFetchError::Malformed))
    );
    assert!(got.observations[0]
        .samples
        .iter()
        .any(|s| s.metric == "if_oper_status"));

    let (origin, _, seen) = serve(vec![Reply::ok(r#"[{"serial":"Q2SW-0001"}]"#)]).await;
    let got = collect(&switch_ports_spec(false), TIMEOUT, Some(&origin)).await;
    assert_eq!(got, Err(MerakiFetchError::Malformed));
    assert_eq!(lines(&seen).len(), 1);
}

/// A 500 on the second status page keeps the first page's switches, says why it stopped, and does
/// not count as a failed collect — the Dashboard did answer (ADR-164 決定 18).
#[tokio::test]
async fn a_500_on_the_second_status_page_keeps_the_first() {
    let page_two = format!(
        "{BASE}/api/v1/organizations/1/switch/ports/statuses/bySwitch?perPage=20&startingAfter=Q2SW-0001"
    );
    let first = status_page(&switch_status("Q2SW-0001", "N_1", PORT_1_UP));
    let (origin, _, _) = serve(vec![
        Reply::ok(&first).next(&page_two),
        Reply::json(500, r#"{"errors":["mock"]}"#),
        Reply::ok(&usage_page("Q2SW-0001", "N_1")),
    ])
    .await;
    let got = collect(&switch_ports_spec(false), TIMEOUT, Some(&origin))
        .await
        .expect("a partial collect");
    assert_eq!(serials(&got), vec!["Q2SW-0001"]);
    assert_eq!(got.stopped, Some(MerakiFetchError::Status(500)));
    assert_eq!(got.failure(), None);
}

// ── The wireless tier (ADR-168) ───────────────────────────────────────────────────────────────

/// One access point's clients row, as `wireless/clients/overview/byDevice` lists it.
fn clients_row(serial: &str, network: &str, online: u32) -> String {
    format!(
        r#"{{"network":{{"id":"{network}"}},"counts":{{"byStatus":{{"online":{online}}}}},"serial":"{serial}"}}"#
    )
}

/// Rows in the envelope the clients and SSID listings answer.
fn envelope(rows: &str) -> String {
    format!(r#"{{"items":[{rows}],"meta":{{"counts":{{"items":{{"total":9,"remaining":0}}}}}}}}"#)
}

/// One access point's utilization row (a bare-array element). `bands` is `(band, total, nonWifi)`.
fn util_row(serial: &str, network: &str, bands: &[(&str, f64, f64)]) -> String {
    let by_band: Vec<String> = bands
        .iter()
        .map(|(band, total, non_wifi)| {
            format!(
                r#"{{"band":"{band}","wifi":{{"percentage":{}}},"nonWifi":{{"percentage":{non_wifi}}},"total":{{"percentage":{total}}}}}"#,
                total - non_wifi
            )
        })
        .collect();
    format!(
        r#"{{"serial":"{serial}","mac":"00:18:0a:00:00:01","network":{{"id":"{network}"}},"byBand":[{}]}}"#,
        by_band.join(",")
    )
}

/// One BSS of `wireless/ssids/statuses/byDevice`: `ssid` is `(number, name, enabled)` and `radio`
/// is `(index, band, channel, power)`, the last two as JSON (`null` allowed).
fn bss(ssid: (u32, &str, bool), broadcasting: bool, radio: (&str, &str, &str, &str)) -> String {
    let (number, name, enabled) = ssid;
    let (index, band, channel, power) = radio;
    format!(
        r#"{{"bssid":"00:18:0a:00:00:{number:02x}","ssid":{{"name":"{name}","number":{number},"enabled":{enabled},"advertised":true}},"radio":{{"isBroadcasting":{broadcasting},"band":"{band}","channel":{channel},"channelWidth":20,"power":{power},"index":"{index}"}}}}"#
    )
}

fn ssid_row(serial: &str, network: &str, sets: &[String]) -> String {
    format!(
        r#"{{"serial":"{serial}","name":"ap-01","network":{{"name":"site-a","id":"{network}"}},"basicServiceSets":[{}]}}"#,
        sets.join(",")
    )
}

fn wireless_spec(ssid_statuses: bool) -> MerakiCollectSpec {
    MerakiCollectSpec {
        ssid_statuses,
        ..spec(MerakiTier::Wireless, &["N_1"])
    }
}

/// A device-level reading of one observation.
fn reading(obs: &crate::MerakiObservation, metric: &str) -> Option<f64> {
    obs.samples
        .iter()
        .find(|s| s.metric == metric && s.ifindex.is_none())
        .map(|s| s.value)
}

/// The tier through HTTP, without the SSID read: the clients listing out of its envelope, the
/// utilization over the last five minutes as a bare array, the watched networks kept, both asked of
/// the whole organization — and the slow SSID listing not asked for at all.
#[tokio::test]
async fn the_wireless_tier_reads_clients_and_utilization_and_leaves_the_ssids_for_when_asked() {
    let clients = envelope(&format!(
        "{},{},{}",
        clients_row("Q2AP-0001", "N_1", 12),
        clients_row("Q2AP-0002", "N_1", 0),
        clients_row("Q2AP-0009", "N_9", 4),
    ));
    let util = format!(
        "[{},{},{}]",
        util_row("Q2AP-0001", "N_1", &[("2.4", 37.25, 0.5), ("5", 3.08, 0.0)]),
        // Stopped: the Dashboard lists no band for it.
        util_row("Q2AP-0002", "N_1", &[]),
        util_row("Q2AP-0009", "N_9", &[("5", 50.0, 1.0)]),
    );
    let (origin, _, seen) = serve(vec![Reply::ok(&clients), Reply::ok(&util)]).await;

    let got = collect(&wireless_spec(false), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");
    assert_eq!(got.failure(), None);
    assert_eq!(
        serials(&got),
        vec!["Q2AP-0001", "Q2AP-0002"],
        "N_9 is not watched"
    );

    let one = &got.observations[0];
    assert_eq!(reading(one, "wlan_ap_client_count"), Some(12.0));
    assert_eq!(
        reading(one, "wlan_ap_ssid_count"),
        None,
        "not read this time"
    );
    let radios: Vec<_> = one
        .radios
        .iter()
        .map(|r| (r.slot, r.channel_util_pct, r.non_wifi_util_pct, r.channel))
        .collect();
    assert_eq!(
        radios,
        [
            (1, Some(37.25), Some(0.5), None),
            (2, Some(3.08), Some(0.0), None)
        ],
        "a fraction is kept as it came, and the channel waits for the SSID read"
    );

    let two = &got.observations[1];
    assert_eq!(
        reading(two, "wlan_ap_client_count"),
        Some(0.0),
        "a stopped access point's zero is the truth about it"
    );
    assert!(two.radios.is_empty(), "no band measured, no radio");

    let sent = lines(&seen);
    assert_eq!(
        sent,
        [
            "GET /api/v1/organizations/1/wireless/clients/overview/byDevice?perPage=1000 HTTP/1.1",
            "GET /api/v1/organizations/1/wireless/devices/channelUtilization/byDevice?interval=300&timespan=300&perPage=1000 HTTP/1.1",
        ]
    );
}

/// 決定 1 and 4: the SSID read is made when core asks, at 250 a page, paged through — and a
/// stopped access point, which the Dashboard still answers as broadcasting, gets nothing from it.
#[tokio::test]
async fn the_ssid_read_counts_what_a_measured_access_point_broadcasts_and_skips_a_stopped_one() {
    let clients = envelope(&format!(
        "{},{}",
        clients_row("Q2AP-0001", "N_1", 12),
        clients_row("Q2AP-0002", "N_1", 0),
    ));
    let util = format!(
        "[{},{}]",
        util_row("Q2AP-0001", "N_1", &[("2.4", 37.25, 0.5), ("5", 3.08, 0.0)]),
        util_row("Q2AP-0002", "N_1", &[]),
    );
    let page_two = format!(
        "{BASE}/api/v1/organizations/1/wireless/ssids/statuses/byDevice?perPage=250&startingAfter=Q2AP-0001"
    );
    let r24 = ("0", "2.4", "6", "17");
    let r5 = ("1", "5", "44", "null");
    let first = envelope(&ssid_row(
        "Q2AP-0001",
        "N_1",
        &[
            // corp on both radios, guest enabled but on air nowhere, iot on 5 GHz only.
            bss((1, "corp", true), true, r24),
            bss((1, "corp", true), true, r5),
            bss((2, "guest", true), false, r24),
            bss((2, "guest", true), false, r5),
            bss((3, "iot", true), true, r5),
        ],
    ));
    let second = envelope(&ssid_row(
        "Q2AP-0002",
        "N_1",
        &[bss((1, "corp", true), true, r24)],
    ));
    let (origin, _, seen) = serve(vec![
        Reply::ok(&clients),
        Reply::ok(&util),
        Reply::ok(&first).next(&page_two),
        Reply::ok(&second),
    ])
    .await;

    let got = collect(&wireless_spec(true), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");
    assert_eq!(got.failure(), None);

    let one = &got.observations[0];
    assert_eq!(
        reading(one, "wlan_ap_ssid_count"),
        Some(2.0),
        "corp and iot"
    );
    let radios: Vec<_> = one
        .radios
        .iter()
        .map(|r| (r.slot, r.channel, r.tx_power_dbm, r.channel_util_pct))
        .collect();
    assert_eq!(
        radios,
        [
            (1, Some(6), Some(17.0), Some(37.25)),
            (2, Some(44), None, Some(3.08)),
        ],
        "a null power is left out, never zero"
    );

    let two = &got.observations[1];
    assert_eq!(
        reading(two, "wlan_ap_ssid_count"),
        None,
        "a stopped access point is still answered as broadcasting; that is not a reading"
    );
    assert!(two.radios.is_empty());

    let sent = lines(&seen);
    assert_eq!(sent.len(), 4, "{sent:?}");
    assert_eq!(
        sent[2],
        "GET /api/v1/organizations/1/wireless/ssids/statuses/byDevice?perPage=250 HTTP/1.1"
    );
    assert!(sent[3].contains("startingAfter=Q2AP-0001"), "{}", sent[3]);
    assert!(sent.iter().all(|l| !l.contains("networkIds")), "{sent:?}");
}

/// The shape each listing answers is its own: a bare array where the clients' envelope belongs fails
/// the collect (it is the tier's first read), and an envelope where the utilization's bare array
/// belongs costs that listing only — named, with the clients kept (ADR-164 決定 25).
#[tokio::test]
async fn a_wireless_listing_in_the_wrong_shape_is_malformed() {
    let (origin, _, seen) = serve(vec![Reply::ok(&format!(
        "[{}]",
        clients_row("Q2AP-0001", "N_1", 1)
    ))])
    .await;
    let got = collect(&wireless_spec(false), TIMEOUT, Some(&origin)).await;
    assert_eq!(got, Err(MerakiFetchError::Malformed));
    assert_eq!(lines(&seen).len(), 1);

    let clients = envelope(&clients_row("Q2AP-0001", "N_1", 3));
    let util = envelope(&util_row("Q2AP-0001", "N_1", &[("5", 1.0, 0.0)]));
    let (origin, _, _) = serve(vec![Reply::ok(&clients), Reply::ok(&util)]).await;
    let got = collect(&wireless_spec(false), TIMEOUT, Some(&origin))
        .await
        .expect("the clients answered");
    assert_eq!(
        got.failed_listing,
        Some((
            MerakiListing::WirelessChannelUtilization,
            MerakiFetchError::Malformed
        ))
    );
    assert_eq!(
        reading(&got.observations[0], "wlan_ap_client_count"),
        Some(3.0)
    );
    assert!(got.observations[0].radios.is_empty());
}

/// A failed SSID read costs its own readings and names itself; the clients and the utilization it
/// came after still go out (ADR-164 決定 25).
#[tokio::test]
async fn a_failed_ssid_read_keeps_the_utilization_and_names_itself() {
    let clients = envelope(&clients_row("Q2AP-0001", "N_1", 3));
    let util = format!("[{}]", util_row("Q2AP-0001", "N_1", &[("2.4", 9.5, 0.0)]));
    let (origin, _, _) = serve(vec![
        Reply::ok(&clients),
        Reply::ok(&util),
        Reply::json(500, r#"{"errors":["mock"]}"#),
    ])
    .await;
    let got = collect(&wireless_spec(true), TIMEOUT, Some(&origin))
        .await
        .expect("a collect");
    assert_eq!(
        got.failed_listing,
        Some((
            MerakiListing::WirelessSsidStatuses,
            MerakiFetchError::Status(500)
        ))
    );
    let one = &got.observations[0];
    assert_eq!(one.radios.len(), 1);
    assert_eq!(one.radios[0].channel_util_pct, Some(9.5));
    assert_eq!(one.radios[0].channel, None);
    assert_eq!(reading(one, "wlan_ap_ssid_count"), None);
}
