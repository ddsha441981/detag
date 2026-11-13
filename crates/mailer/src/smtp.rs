//! detag-mailer::smtp — SMTP transport (P1-028).
//!
//! [`MailTransport`] is the drain's send seam: the production
//! implementation is [`LettreTransport`] (lettre, async SMTP),
//! tests use [`MemoryTransport`] (records what would be sent,
//! optionally fails the first N sends so retry/backoff is
//! testable without a relay). The trait is generic-friendly on
//! purpose — no `async_trait` dependency: callers take `T:
//! MailTransport` instead of a trait object.
//!
//! Wiring (from `PF_SMTP_*`): plaintext to `host:port` when
//! `PF_SMTP_TLS` is off (dev MailHog on :1025); STARTTLS via
//! `starttls_relay` when it is on (port still from config — a
//! relay on 587 is the usual shape). Credentials are sent only
//! when `PF_SMTP_USER` is non-empty; MailHog needs none.

use crate::error::MailError;
use detag_core::config::Settings;
use lettre::message::{Mailbox, Message, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use std::sync::Mutex;

/// One mail to send: the rendered template + the envelope.
/// The drain builds this from the outbox row; the transport
/// never sees the payload, the token, or the user id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMail {
    /// Recipient address (resolved by the drain, never logged).
    pub to: String,
    /// Sender address (PF_MAIL_FROM).
    pub from: String,
    /// Rendered subject (single line).
    pub subject: String,
    /// Rendered plain-text body.
    pub text_body: String,
    /// Rendered HTML body (P1-029) — sent as the HTML part of a
    /// multipart/alternative message next to the text part.
    pub html_body: String,
}

/// SMTP configuration (from `Settings`, see PF_SMTP_*).
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub from: String,
    pub tls: bool,
}

impl SmtpConfig {
    /// From loaded settings (P1-028): the worker builds this
    /// once at boot, so a bad `PF_MAIL_FROM` fails fast instead
    /// of parking every queued mail.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            host: settings.smtp_host.clone(),
            port: settings.smtp_port,
            user: settings.smtp_user.clone(),
            pass: settings.smtp_pass.clone(),
            from: settings.mail_from.clone(),
            tls: settings.smtp_tls,
        }
    }
}

/// Send one mail. Implemented by [`LettreTransport`] (real
/// SMTP) and [`MemoryTransport`] (tests). A send failure is
/// [`MailError::Smtp`] unless the address itself is bad
/// ([`MailError::InvalidAddress`], permanent).
pub trait MailTransport: Send + Sync {
    fn send(
        &self,
        mail: &OutgoingMail,
    ) -> impl std::future::Future<Output = Result<(), MailError>> + Send;
}

/// The production transport: lettre's async SMTP client.
/// Built once per process (the pool is inside); sending opens
/// connections lazily, so building never touches the network —
/// a down relay is a per-mail failure the drain retries, not a
/// boot error.
pub struct LettreTransport {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

// Manual Debug: lettre's AsyncSmtpTransport has no Debug impl,
// so the derive cannot work — print the sender only (never
// credentials; there are none stored here anyway).
impl std::fmt::Debug for LettreTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LettreTransport")
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

impl LettreTransport {
    /// Build the transport from config. Parses `from` now (a
    /// bad sender fails boot, not the first mail) and picks
    /// STARTTLS when `tls` is on, plaintext otherwise.
    /// Credentials are attached only when `user` is non-empty.
    pub fn new(config: &SmtpConfig) -> Result<Self, MailError> {
        let from = config
            .from
            .parse::<Mailbox>()
            .map_err(|_| MailError::InvalidAddress(config.from.clone()))?;
        let mut builder = if config.tls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                .map_err(|err| MailError::Config(err.to_string()))?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
        };
        builder = builder.port(config.port);
        if !config.user.is_empty() {
            builder =
                builder.credentials(Credentials::new(config.user.clone(), config.pass.clone()));
        }
        Ok(Self {
            transport: builder.build(),
            from,
        })
    }
}

impl MailTransport for LettreTransport {
    async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        // `mail.from` must equal the transport's sender: the
        // drain always passes the configured PF_MAIL_FROM, and
        // a mismatch here would mean a bug in the drain, not a
        // per-mail choice — so the transport sends from its own
        // parsed `from` and only validates the recipient.
        let to = mail
            .to
            .parse::<Mailbox>()
            .map_err(|_| MailError::InvalidAddress(mail.to.clone()))?;
        let message = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(&mail.subject)
            .multipart(MultiPart::alternative_plain_html(
                mail.text_body.clone(),
                mail.html_body.clone(),
            ))
            .map_err(|err| MailError::Smtp(err.to_string()))?;
        self.transport
            .send(message)
            .await
            .map_err(|err| MailError::Smtp(err.to_string()))?;
        Ok(())
    }
}

/// In-memory fake transport for tests: records every mail it
/// "sends" and can be told to fail the first N sends, so the
/// drain's retry/backoff is testable without a relay. `Send +
/// Sync` via interior mutability — the drain holds a shared
/// reference while tests read back what was sent.
#[derive(Debug, Default)]
pub struct MemoryTransport {
    sent: Mutex<Vec<OutgoingMail>>,
    failures_left: Mutex<usize>,
}

impl MemoryTransport {
    /// An empty fake: sends succeed and are recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fail the next `n` sends with a synthetic SMTP error,
    /// then succeed. A test that wants "fail twice, then send"
    /// calls `fail_next(2)`.
    pub fn fail_next(&self, n: usize) {
        *self.failures_left.lock().expect("memory transport lock") = n;
    }

    /// The mails recorded so far, in send order.
    pub fn sent(&self) -> Vec<OutgoingMail> {
        self.sent.lock().expect("memory transport lock").clone()
    }

    /// How many mails were recorded.
    pub fn sent_count(&self) -> usize {
        self.sent.lock().expect("memory transport lock").len()
    }
}

impl MailTransport for MemoryTransport {
    async fn send(&self, mail: &OutgoingMail) -> Result<(), MailError> {
        let mut failures = self.failures_left.lock().expect("memory transport lock");
        if *failures > 0 {
            *failures -= 1;
            return Err(MailError::Smtp("injected test failure".to_string()));
        }
        drop(failures);
        self.sent
            .lock()
            .expect("memory transport lock")
            .push(mail.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    /// A minimal fake SMTP server: answers the greeting, EHLO,
    /// MAIL FROM, RCPT TO, DATA (captures the message) and QUIT
    /// — just enough SMTP to prove the lettre transport really
    /// speaks the protocol. Runs on a loopback ephemeral port;
    /// `capture` receives the raw DATA bytes (headers + body).
    async fn run_fake_smtp(capture: Arc<Mutex<Option<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake smtp binds");
        let port = listener.local_addr().expect("fake smtp port").port();
        capture
            .lock()
            .expect("capture lock")
            .replace(format!("PORT:{port}"));
        let (mut stream, _) = listener.accept().await.expect("fake smtp accept");
        let (reader, mut writer) = stream.split();
        let mut lines = BufReader::new(reader).lines();
        writer
            .write_all(b"220 localhost fake-smtp\r\n")
            .await
            .expect("fake smtp greeting");
        while let Some(line) = lines.next_line().await.expect("fake smtp read") {
            let upper = line.to_uppercase();
            if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                writer
                    .write_all(b"250-localhost\r\n250 SMTPUTF8\r\n")
                    .await
                    .expect("fake smtp EHLO");
            } else if upper.starts_with("MAIL FROM") {
                writer
                    .write_all(b"250 OK\r\n")
                    .await
                    .expect("fake smtp MAIL");
            } else if upper.starts_with("RCPT TO") {
                writer
                    .write_all(b"250 OK\r\n")
                    .await
                    .expect("fake smtp RCPT");
            } else if upper.starts_with("DATA") {
                writer
                    .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                    .await
                    .expect("fake smtp DATA");
                let mut data = String::new();
                while let Some(data_line) = lines.next_line().await.expect("fake smtp data") {
                    if data_line == "." {
                        break;
                    }
                    data.push_str(&data_line);
                    data.push('\n');
                }
                *capture.lock().expect("capture lock") = Some(data);
                writer
                    .write_all(b"250 OK\r\n")
                    .await
                    .expect("fake smtp queued");
            } else if upper.starts_with("QUIT") {
                writer
                    .write_all(b"221 Bye\r\n")
                    .await
                    .expect("fake smtp QUIT");
                break;
            } else if upper.starts_with("RSET") {
                writer
                    .write_all(b"250 OK\r\n")
                    .await
                    .expect("fake smtp RSET");
            } else {
                writer
                    .write_all(b"500 unrecognized\r\n")
                    .await
                    .expect("fake smtp unknown");
            }
        }
    }

    #[tokio::test]
    async fn lettre_transport_sends_a_real_smtp_message() {
        let capture: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let server = tokio::spawn(run_fake_smtp(Arc::clone(&capture)));
        // The server publishes its port in the capture slot
        // before accepting; wait for it (no sleep loop in the
        // test would be racy — poll briefly instead).
        let port = {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let marker = capture.lock().expect("capture lock").clone();
                if let Some(marker) = marker {
                    break marker
                        .strip_prefix("PORT:")
                        .expect("port marker")
                        .parse::<u16>()
                        .expect("port parses");
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "fake smtp did not start in 5 s"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        let transport = LettreTransport::new(&SmtpConfig {
            host: "127.0.0.1".to_string(),
            port,
            user: String::new(),
            pass: String::new(),
            from: "noreply@detag.local".to_string(),
            tls: false,
        })
        .expect("plaintext transport builds");
        transport
            .send(&OutgoingMail {
                to: "user@example.com".to_string(),
                from: "noreply@detag.local".to_string(),
                subject: "Verify your Detag email".to_string(),
                text_body: "open the link".to_string(),
                html_body: "<p>open the link</p>".to_string(),
            })
            .await
            .expect("send to the fake relay succeeds");
        server.await.expect("fake smtp task finishes");
        let data = capture
            .lock()
            .expect("capture lock")
            .clone()
            .expect("the DATA payload was captured");
        assert!(
            data.contains("To: user@example.com"),
            "DATA must carry the recipient:\n{data}"
        );
        assert!(
            data.contains("From: noreply@detag.local"),
            "DATA must carry the sender:\n{data}"
        );
        assert!(
            data.contains("Subject: Verify your Detag email"),
            "DATA must carry the subject:\n{data}"
        );
        assert!(
            data.contains("open the link"),
            "DATA must carry the body:\n{data}"
        );
        assert!(
            data.contains("multipart/alternative"),
            "DATA must be multipart (text + html):\n{data}"
        );
        assert!(
            data.contains("<p>open the link</p>"),
            "DATA must carry the html part:\n{data}"
        );
    }

    #[tokio::test]
    async fn lettre_transport_rejects_a_bad_recipient_without_touching_the_relay() {
        let transport = LettreTransport::new(&SmtpConfig {
            host: "127.0.0.1".to_string(),
            port: 1,
            user: String::new(),
            pass: String::new(),
            from: "noreply@detag.local".to_string(),
            tls: false,
        })
        .expect("transport builds");
        let err = transport
            .send(&OutgoingMail {
                to: "not-an-email".to_string(),
                from: "noreply@detag.local".to_string(),
                subject: "x".to_string(),
                text_body: "y".to_string(),
                html_body: "<p>y</p>".to_string(),
            })
            .await
            .expect_err("a bad recipient must fail before any SMTP");
        assert!(
            matches!(err, MailError::InvalidAddress(_)),
            "bad recipient is a permanent address error, got {err}"
        );
        assert!(err.is_permanent());
    }

    #[tokio::test]
    async fn lettre_transport_to_a_dead_relay_is_a_transient_smtp_error() {
        // Port 1 on loopback refuses connections; the error must
        // be Smtp (transient — the drain retries), not a panic
        // or a permanent classification.
        let transport = LettreTransport::new(&SmtpConfig {
            host: "127.0.0.1".to_string(),
            port: 1,
            user: String::new(),
            pass: String::new(),
            from: "noreply@detag.local".to_string(),
            tls: false,
        })
        .expect("transport builds");
        let err = transport
            .send(&OutgoingMail {
                to: "user@example.com".to_string(),
                from: "noreply@detag.local".to_string(),
                subject: "x".to_string(),
                text_body: "y".to_string(),
                html_body: "<p>y</p>".to_string(),
            })
            .await
            .expect_err("a dead relay must fail the send");
        assert!(
            matches!(err, MailError::Smtp(_)),
            "dead relay is a transient SMTP error, got {err}"
        );
        assert!(!err.is_permanent());
    }

    #[test]
    fn transport_rejects_a_bad_sender_at_build_time() {
        let err = LettreTransport::new(&SmtpConfig {
            host: "127.0.0.1".to_string(),
            port: 1025,
            user: String::new(),
            pass: String::new(),
            from: "not-an-email".to_string(),
            tls: false,
        })
        .expect_err("a bad PF_MAIL_FROM must fail boot, not the first mail");
        assert!(matches!(err, MailError::InvalidAddress(_)));
    }

    #[tokio::test]
    async fn memory_transport_records_and_fails_on_demand() {
        let transport = MemoryTransport::new();
        transport.fail_next(2);
        let mail = OutgoingMail {
            to: "user@example.com".to_string(),
            from: "noreply@detag.local".to_string(),
            subject: "s".to_string(),
            text_body: "b".to_string(),
            html_body: "<p>b</p>".to_string(),
        };
        assert!(transport.send(&mail).await.is_err(), "1st send fails");
        assert!(transport.send(&mail).await.is_err(), "2nd send fails");
        transport.send(&mail).await.expect("3rd send succeeds");
        assert_eq!(transport.sent_count(), 1, "only the success is recorded");
        assert_eq!(transport.sent()[0], mail);
    }
}
