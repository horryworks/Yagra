// SPDX-License-Identifier: AGPL-3.0-only
//! The Redis protocol (RESP2), only as far as the poller-state mirror needs it (ADR-213 decision 3).
//!
//! `volatile.rs` sends three things — `SET … EX`, a `MULTI`/`DEL`/`HSET`/`EXEC` block and `PING` —
//! and nothing else in the program talks to Redis. That is small and closed enough to own, which
//! removed the `redis` crate (and `arc-swap`, which only it used). What this module deliberately does not
//! do: TLS (`rediss://`), unix sockets, RESP3, pub/sub, cluster redirects. A URL asking for any of
//! them is refused, and the mirror then runs degraded exactly as it does with no URL at all.
//!
//! Every reply is read with a cap on its size, because a reply is bytes off a socket: an endless
//! line or a bulk length of 2^62 must end in an error, never in an allocation.

use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// The default Redis port, when the URL names none.
const DEFAULT_PORT: u16 = 6379;
/// Longest status/error/integer line accepted.
const MAX_LINE: usize = 64 * 1024;
/// Largest bulk string accepted. Nothing the mirror reads back is more than a few bytes.
const MAX_BULK: usize = 16 * 1024 * 1024;
/// Most elements accepted in one array reply (an `EXEC` answers one per queued command).
const MAX_ARRAY: usize = 1024;

/// Why a Redis exchange failed. Never carries the password.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RespError {
    #[error("invalid Redis URL: {0}")]
    Url(&'static str),
    #[error("Redis I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("Redis did not answer within {0:?}")]
    Timeout(Duration),
    #[error("Redis protocol: {0}")]
    Protocol(&'static str),
    #[error("Redis error reply: {0}")]
    Server(String),
}

/// Where to connect and how to sign in, parsed from `redis://[user[:pass]@]host[:port][/db]`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RedisTarget {
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    db: u32,
}

// Hand-written so a `{:?}` in a log can never print the password.
impl std::fmt::Debug for RedisTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisTarget")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("db", &self.db)
            .finish()
    }
}

impl RedisTarget {
    /// Parse a `redis://` URL. Userinfo is percent-decoded, as the `redis` crate did.
    pub(crate) fn parse(url: &str) -> Result<Self, RespError> {
        let rest = url
            .trim()
            .strip_prefix("redis://")
            .ok_or(RespError::Url("only redis:// is supported"))?;
        if rest.contains('?') || rest.contains('#') {
            return Err(RespError::Url("query parameters are not supported"));
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (username, password) = match userinfo {
            None => (None, None),
            Some(info) => {
                let (user, pass) = match info.find(':') {
                    Some(i) => (&info[..i], Some(&info[i + 1..])),
                    None => (info, None),
                };
                let user = percent_decode(user)?;
                let pass = pass.map(percent_decode).transpose()?;
                (Some(user).filter(|u| !u.is_empty()), pass)
            }
        };
        let (host, port) = split_host_port(hostport)?;
        let db = match path {
            "" => 0,
            db => db
                .parse()
                .map_err(|_| RespError::Url("bad database number"))?,
        };
        Ok(Self {
            host,
            port,
            username,
            password,
            db,
        })
    }
}

fn split_host_port(hostport: &str) -> Result<(String, u16), RespError> {
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let end = v6
            .find(']')
            .ok_or(RespError::Url("unclosed IPv6 bracket"))?;
        let after = &v6[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(RespError::Url("junk after IPv6 address")),
        };
        (&v6[..end], port)
    } else {
        match hostport.rfind(':') {
            Some(i) => (&hostport[..i], Some(&hostport[i + 1..])),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err(RespError::Url("no host"));
    }
    let port = match port {
        None => DEFAULT_PORT,
        Some(p) => p.parse().map_err(|_| RespError::Url("bad port"))?,
    };
    Ok((host.to_owned(), port))
}

fn percent_decode(s: &str) -> Result<String, RespError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s
                .get(i + 1..i + 3)
                .ok_or(RespError::Url("truncated percent escape"))?;
            out.push(
                u8::from_str_radix(hex, 16).map_err(|_| RespError::Url("bad percent escape"))?,
            );
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| RespError::Url("userinfo is not UTF-8"))
}

/// One reply, as RESP2 spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reply {
    Simple(String),
    Error(String),
    Integer(i64),
    /// `None` is the nil bulk string.
    Bulk(Option<Vec<u8>>),
    /// `None` is the nil array — what `EXEC` answers when the transaction was aborted.
    Array(Option<Vec<Reply>>),
}

impl Reply {
    /// The reply as a failure, if it is one: an error reply, or an error inside an array (an
    /// `EXEC` reports a failed queued command that way, and still answers `*N`).
    fn failure(&self) -> Option<&str> {
        match self {
            Reply::Error(e) => Some(e),
            Reply::Array(Some(items)) => items.iter().find_map(Reply::failure),
            Reply::Simple(_) | Reply::Integer(_) | Reply::Bulk(_) | Reply::Array(None) => None,
        }
    }
}

/// Append one command to `out` as a RESP array of bulk strings.
pub(crate) fn encode(args: &[&[u8]], out: &mut Vec<u8>) {
    out.extend_from_slice(format!("*{}\r\n", args.len()).as_bytes());
    for arg in args {
        out.extend_from_slice(format!("${}\r\n", arg.len()).as_bytes());
        out.extend_from_slice(arg);
        out.extend_from_slice(b"\r\n");
    }
}

/// Read one line up to CRLF, without the CRLF, refusing one longer than [`MAX_LINE`].
async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<String, RespError> {
    let mut line = Vec::new();
    let n = (&mut *r)
        .take(MAX_LINE as u64 + 2)
        .read_until(b'\n', &mut line)
        .await?;
    if n == 0 {
        return Err(RespError::Protocol("connection closed"));
    }
    if !line.ends_with(b"\r\n") {
        return Err(RespError::Protocol("line too long or not CRLF-terminated"));
    }
    line.truncate(line.len() - 2);
    String::from_utf8(line).map_err(|_| RespError::Protocol("line is not UTF-8"))
}

/// A signed length: `-1` is nil, anything below that or above `cap` is refused.
fn length(text: &str, cap: usize) -> Result<Option<usize>, RespError> {
    let n: i64 = text
        .parse()
        .map_err(|_| RespError::Protocol("bad length"))?;
    if n == -1 {
        return Ok(None);
    }
    let n = usize::try_from(n).map_err(|_| RespError::Protocol("negative length"))?;
    if n > cap {
        return Err(RespError::Protocol("reply larger than this client accepts"));
    }
    Ok(Some(n))
}

/// Read one scalar reply; an array header is handed back as `Err(len)` for the caller to fill.
async fn read_scalar<R: AsyncBufRead + Unpin>(
    r: &mut R,
) -> Result<Result<Reply, Option<usize>>, RespError> {
    let line = read_line(r).await?;
    let (kind, rest) = line.split_at(line.chars().next().map_or(0, char::len_utf8));
    Ok(match kind {
        "+" => Ok(Reply::Simple(rest.to_owned())),
        "-" => Ok(Reply::Error(rest.to_owned())),
        ":" => Ok(Reply::Integer(
            rest.parse()
                .map_err(|_| RespError::Protocol("bad integer"))?,
        )),
        "$" => match length(rest, MAX_BULK)? {
            None => Ok(Reply::Bulk(None)),
            Some(n) => {
                let mut buf = vec![0u8; n + 2];
                r.read_exact(&mut buf).await?;
                if !buf.ends_with(b"\r\n") {
                    return Err(RespError::Protocol("bulk string not CRLF-terminated"));
                }
                buf.truncate(n);
                Ok(Reply::Bulk(Some(buf)))
            }
        },
        "*" => Err(length(rest, MAX_ARRAY)?),
        _ => return Err(RespError::Protocol("unknown reply type")),
    })
}

/// Read one reply. Arrays nest one level here (an `EXEC` answer holds scalars), and a nested
/// array is read the same way, iteratively, so a hostile reply cannot recurse without bound.
pub(crate) async fn read_reply<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Reply, RespError> {
    // Each frame is an array being filled: (expected length, items so far).
    let mut stack: Vec<(usize, Vec<Reply>)> = Vec::new();
    loop {
        let mut reply = match read_scalar(r).await? {
            Ok(scalar) => scalar,
            Err(None) => Reply::Array(None),
            Err(Some(0)) => Reply::Array(Some(Vec::new())),
            Err(Some(n)) => {
                if stack.len() >= 8 {
                    return Err(RespError::Protocol("arrays nested too deep"));
                }
                stack.push((n, Vec::with_capacity(n)));
                continue;
            }
        };
        // Fold the finished reply into the arrays waiting for it.
        loop {
            match stack.last_mut() {
                None => return Ok(reply),
                Some((want, items)) => {
                    items.push(reply);
                    if items.len() < *want {
                        break;
                    }
                    let (_, items) = stack.pop().expect("just looked at it");
                    reply = Reply::Array(Some(items));
                }
            }
        }
    }
}

/// One open connection, signed in and on the right database.
pub(crate) struct Connection {
    stream: BufReader<TcpStream>,
}

impl Connection {
    /// Connect, then `AUTH` and `SELECT` as the target asks, all inside `timeout`.
    pub(crate) async fn open(target: &RedisTarget, timeout: Duration) -> Result<Self, RespError> {
        tokio::time::timeout(timeout, async {
            let tcp = TcpStream::connect((target.host.as_str(), target.port)).await?;
            tcp.set_nodelay(true)?;
            let mut conn = Self {
                stream: BufReader::new(tcp),
            };
            let mut setup: Vec<Vec<Vec<u8>>> = Vec::new();
            match (&target.username, &target.password) {
                (Some(user), Some(pass)) => setup.push(vec![
                    b"AUTH".to_vec(),
                    user.as_bytes().to_vec(),
                    pass.as_bytes().to_vec(),
                ]),
                (None, Some(pass)) => setup.push(vec![b"AUTH".to_vec(), pass.as_bytes().to_vec()]),
                (_, None) => {}
            }
            if target.db != 0 {
                setup.push(vec![b"SELECT".to_vec(), target.db.to_string().into_bytes()]);
            }
            if !setup.is_empty() {
                conn.exchange_inner(&setup).await?;
            }
            Ok(conn)
        })
        .await
        .map_err(|_| RespError::Timeout(timeout))?
    }

    /// Send `commands` in one write and read one reply per command, inside `timeout`. Any error
    /// reply — including one inside an `EXEC` answer — is an `Err`.
    pub(crate) async fn exchange(
        &mut self,
        commands: &[Vec<Vec<u8>>],
        timeout: Duration,
    ) -> Result<Vec<Reply>, RespError> {
        tokio::time::timeout(timeout, self.exchange_inner(commands))
            .await
            .map_err(|_| RespError::Timeout(timeout))?
    }

    async fn exchange_inner(&mut self, commands: &[Vec<Vec<u8>>]) -> Result<Vec<Reply>, RespError> {
        let mut out = Vec::new();
        for cmd in commands {
            let args: Vec<&[u8]> = cmd.iter().map(Vec::as_slice).collect();
            encode(&args, &mut out);
        }
        self.stream.get_mut().write_all(&out).await?;
        let mut replies = Vec::with_capacity(commands.len());
        for _ in commands {
            replies.push(read_reply(&mut self.stream).await?);
        }
        if let Some(e) = replies.iter().find_map(Reply::failure) {
            return Err(RespError::Server(e.to_owned()));
        }
        Ok(replies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> RedisTarget {
        RedisTarget::parse(url).expect(url)
    }

    #[test]
    fn the_deployment_url_parses_to_host_and_default_port() {
        let t = target("redis://redis:6379");
        assert_eq!((t.host.as_str(), t.port, t.db), ("redis", 6379, 0));
        assert_eq!((t.username, t.password), (None, None));
        assert_eq!(target("redis://redis").port, DEFAULT_PORT);
    }

    #[test]
    fn userinfo_database_and_ipv6_parse() {
        let t = target("redis://:s%40cret@10.0.0.5:6380/2");
        assert_eq!((t.host.as_str(), t.port, t.db), ("10.0.0.5", 6380, 2));
        assert_eq!(t.username, None);
        assert_eq!(t.password.as_deref(), Some("s@cret"));
        let t = target("redis://mirror:pw@[2001:db8::1]:6379");
        assert_eq!(t.host, "2001:db8::1");
        assert_eq!(t.username.as_deref(), Some("mirror"));
        assert_eq!(target("redis://[2001:db8::1]").port, DEFAULT_PORT);
    }

    #[test]
    fn what_this_client_does_not_speak_is_refused() {
        for url in [
            "rediss://redis:6379",
            "redis+unix:///run/redis.sock",
            "unix:///run/redis.sock",
            "not a redis url",
            "redis://",
            "redis://host:notaport",
            "redis://host/notadb",
            "redis://host?db=1",
            "redis://[2001:db8::1",
            "redis://:%zz@host",
        ] {
            assert!(RedisTarget::parse(url).is_err(), "{url} should be refused");
        }
    }

    #[test]
    fn the_password_never_reaches_debug_output() {
        let t = target("redis://user:hunter2@redis:6379");
        let shown = format!("{t:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn commands_encode_as_arrays_of_bulk_strings() {
        let mut out = Vec::new();
        encode(&[b"SET", b"k", b"v", b"EX", b"30"], &mut out);
        assert_eq!(
            out,
            b"*5\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n$2\r\nEX\r\n$2\r\n30\r\n"
        );
        // A value carrying CRLF is length-prefixed, so it cannot split the command.
        let mut out = Vec::new();
        encode(&[b"SET", b"k", b"a\r\nb"], &mut out);
        assert!(out.ends_with(b"$4\r\na\r\nb\r\n"));
    }

    async fn parse(bytes: &[u8]) -> Result<Reply, RespError> {
        read_reply(&mut BufReader::new(bytes)).await
    }

    #[tokio::test]
    async fn every_reply_type_reads() {
        assert_eq!(parse(b"+OK\r\n").await.unwrap(), Reply::Simple("OK".into()));
        assert_eq!(
            parse(b"-ERR no\r\n").await.unwrap(),
            Reply::Error("ERR no".into())
        );
        assert_eq!(parse(b":42\r\n").await.unwrap(), Reply::Integer(42));
        assert_eq!(
            parse(b"$5\r\nPO\r\nG\r\n").await.unwrap(),
            Reply::Bulk(Some(b"PO\r\nG".to_vec()))
        );
        assert_eq!(parse(b"$-1\r\n").await.unwrap(), Reply::Bulk(None));
        assert_eq!(parse(b"*-1\r\n").await.unwrap(), Reply::Array(None));
        assert_eq!(parse(b"*0\r\n").await.unwrap(), Reply::Array(Some(vec![])));
    }

    /// The `EXEC` answer of `write_assignments`: one reply per queued command.
    #[tokio::test]
    async fn an_exec_answer_reads_as_one_array() {
        let r = parse(b"*2\r\n:1\r\n:3\r\n").await.unwrap();
        assert_eq!(
            r,
            Reply::Array(Some(vec![Reply::Integer(1), Reply::Integer(3)]))
        );
        assert_eq!(r.failure(), None);
        let nested = parse(b"*2\r\n*1\r\n+a\r\n:2\r\n").await.unwrap();
        assert_eq!(
            nested,
            Reply::Array(Some(vec![
                Reply::Array(Some(vec![Reply::Simple("a".into())])),
                Reply::Integer(2)
            ]))
        );
    }

    /// 🚨 A queued command that failed is reported **inside** the `EXEC` array, after a `+QUEUED`
    /// that looked fine. Reading only the top level would call that transaction a success.
    #[tokio::test]
    async fn an_error_inside_an_exec_answer_is_a_failure() {
        let r = parse(b"*2\r\n:1\r\n-WRONGTYPE bad\r\n").await.unwrap();
        assert_eq!(r.failure(), Some("WRONGTYPE bad"));
    }

    #[tokio::test]
    async fn oversized_or_malformed_replies_are_errors_not_allocations() {
        for bytes in [
            &b"$99999999999\r\n"[..],
            b"*99999999\r\n",
            b"$-5\r\n",
            b"?what\r\n",
            b"+no crlf",
            b"$3\r\nab",
            b":12x\r\n",
            b"",
        ] {
            assert!(parse(bytes).await.is_err(), "{bytes:?} should be refused");
        }
        let long = format!("+{}\r\n", "x".repeat(MAX_LINE + 10));
        assert!(parse(long.as_bytes()).await.is_err());
        let deep = "*1\r\n".repeat(20) + ":1\r\n";
        assert!(parse(deep.as_bytes()).await.is_err());
    }

    /// The whole exchange against a scripted server on a real socket: `AUTH` first, then both
    /// commands in one write with one reply each, and an error reply surfacing as `Err`.
    #[tokio::test]
    async fn an_exchange_pipelines_and_reports_error_replies() {
        let wire = |cmds: &[&[&[u8]]]| {
            let mut out = Vec::new();
            for c in cmds {
                encode(c, &mut out);
            }
            out
        };
        let auth = wire(&[&[b"AUTH", b"pw"]]);
        let pair = wire(&[&[b"SET", b"k", b"v"], &[b"PING"]]);
        let ping = wire(&[&[b"PING"]]);
        let (auth_len, pair_len, ping_len) = (auth.len(), pair.len(), ping.len());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut heard = Vec::new();
            for (len, answer) in [
                (auth_len, &b"+OK\r\n"[..]),
                (pair_len, b"+OK\r\n+PONG\r\n"),
                (ping_len, b"-NOAUTH Authentication required.\r\n"),
            ] {
                let mut buf = vec![0u8; len];
                sock.read_exact(&mut buf).await.unwrap();
                heard.push(buf);
                sock.write_all(answer).await.unwrap();
            }
            heard
        });

        let t = target(&format!("redis://:pw@127.0.0.1:{port}"));
        let mut conn = Connection::open(&t, Duration::from_secs(2)).await.unwrap();
        let replies = conn
            .exchange(
                &[
                    vec![b"SET".to_vec(), b"k".to_vec(), b"v".to_vec()],
                    vec![b"PING".to_vec()],
                ],
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(
            replies,
            vec![Reply::Simple("OK".into()), Reply::Simple("PONG".into())]
        );
        let err = conn
            .exchange(&[vec![b"PING".to_vec()]], Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(
            matches!(err, RespError::Server(ref m) if m.starts_with("NOAUTH")),
            "{err}"
        );
        assert_eq!(server.await.unwrap(), vec![auth, pair, ping]);
    }

    #[tokio::test]
    async fn a_server_that_never_answers_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _hold = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(sock);
        });
        let t = target(&format!("redis://127.0.0.1:{port}"));
        let mut conn = Connection::open(&t, Duration::from_secs(2)).await.unwrap();
        let err = conn
            .exchange(&[vec![b"PING".to_vec()]], Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(matches!(err, RespError::Timeout(_)), "{err}");
    }
}
