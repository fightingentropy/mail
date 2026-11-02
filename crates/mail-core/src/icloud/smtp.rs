//! Sending through iCloud's SMTP server: STARTTLS on port 587, then the few
//! commands one message needs. Only `IcloudClient::send_draft` sends, which
//! only the window calls, after you confirm.

use std::io::{BufRead, BufReader, Read, Write};

use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD};

use super::tls;

const HOST: &str = "smtp.mail.me.com";
const PORT: u16 = 587;

/// Sends `raw` from `from` to `recipients`, signing in as `from` with the
/// app-specific password.
pub(super) fn send(
    from: &str,
    password: &str,
    recipients: &[String],
    raw: &[u8],
) -> anyhow::Result<()> {
    let mut plain = Dialogue::new(tls::tcp(HOST, PORT)?);
    plain.expect(220)?;
    plain.command("EHLO mail.local", 250)?;
    plain.command("STARTTLS", 220)?;
    let tcp = plain.reader.into_inner();
    let mut secure = Dialogue::new(tls::wrap(tcp, HOST)?);
    deliver(&mut secure, from, password, recipients, raw)
}

/// Everything after TLS is up.
fn deliver<S: Read + Write>(
    dialogue: &mut Dialogue<S>,
    from: &str,
    password: &str,
    recipients: &[String],
    raw: &[u8],
) -> anyhow::Result<()> {
    anyhow::ensure!(!recipients.is_empty(), "The message has no recipients.");
    for address in std::iter::once(from).chain(recipients.iter().map(String::as_str)) {
        // An address can't carry a command of its own.
        anyhow::ensure!(
            !address.contains(['\r', '\n', '<', '>']),
            "Not an email address: {address}"
        );
    }
    dialogue.command("EHLO mail.local", 250)?;
    let token = STANDARD.encode(format!("\0{from}\0{password}"));
    dialogue
        .send(&format!("AUTH PLAIN {token}"))
        .and_then(|()| dialogue.expect(235))
        .context("iCloud didn't accept the app-specific password for sending")?;
    dialogue.command(&format!("MAIL FROM:<{from}>"), 250)?;
    for recipient in recipients {
        dialogue
            .command(&format!("RCPT TO:<{recipient}>"), 250)
            .with_context(|| format!("iCloud won't send to {recipient}"))?;
    }
    dialogue.command("DATA", 354)?;
    dialogue.data(raw)?;
    dialogue.expect(250)?;
    let _ = dialogue.command("QUIT", 221);
    Ok(())
}

struct Dialogue<S: Read + Write> {
    reader: BufReader<S>,
}

impl<S: Read + Write> Dialogue<S> {
    fn new(stream: S) -> Self {
        Self {
            reader: BufReader::new(stream),
        }
    }

    fn command(&mut self, line: &str, expected: u16) -> anyhow::Result<()> {
        self.send(line)?;
        self.expect(expected)
            .with_context(|| format!("iCloud refused {}", line.split(' ').next().unwrap_or(line)))
    }

    fn send(&mut self, line: &str) -> anyhow::Result<()> {
        let stream = self.reader.get_mut();
        stream.write_all(line.as_bytes())?;
        stream.write_all(b"\r\n")?;
        stream.flush()?;
        Ok(())
    }

    /// Reads a reply, which may span lines (`250-…` then `250 …`), and
    /// fails unless its code is `expected` (251 counts as 250).
    fn expect(&mut self, expected: u16) -> anyhow::Result<()> {
        let mut text = String::new();
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                bail!("The server closed the connection");
            }
            let code: u16 = line
                .get(..3)
                .and_then(|code| code.parse().ok())
                .context("Not an SMTP reply")?;
            text.push_str(line.get(4..).unwrap_or_default().trim_end());
            text.push(' ');
            if line.as_bytes().get(3) != Some(&b'-') {
                if code == expected || (expected == 250 && code == 251) {
                    return Ok(());
                }
                bail!("{code} {}", text.trim());
            }
        }
    }

    /// The message after DATA: lines starting with a dot get another, and
    /// a lone dot ends it.
    fn data(&mut self, raw: &[u8]) -> anyhow::Result<()> {
        let stream = self.reader.get_mut();
        let mut start_of_line = true;
        let mut out = Vec::with_capacity(raw.len() + 64);
        for &byte in raw {
            if start_of_line && byte == b'.' {
                out.push(b'.');
            }
            out.push(byte);
            start_of_line = byte == b'\n';
        }
        if !out.ends_with(b"\r\n") {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b".\r\n");
        stream.write_all(&out)?;
        stream.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};

    use super::{Dialogue, deliver};

    /// A server that replies from a script and records what it was sent.
    struct Scripted {
        replies: Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.replies.read(buf)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.sent.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn server(replies: &str) -> Dialogue<Scripted> {
        Dialogue::new(Scripted {
            replies: Cursor::new(replies.as_bytes().to_vec()),
            sent: Vec::new(),
        })
    }

    #[test]
    fn a_message_goes_out_with_its_dots_doubled() {
        let mut dialogue = server(
            "250-outbound.example\r\n250 CHUNKING\r\n235 ok\r\n250 ok\r\n250 ok\r\n251 forwarding\r\n354 go\r\n250 queued\r\n221 bye\r\n",
        );
        deliver(
            &mut dialogue,
            "me@icloud.com",
            "pass",
            &["ana@example.org".into(), "bo@example.net".into()],
            b"Subject: Hi\r\n\r\n.hidden\r\nok\r\n",
        )
        .unwrap();
        let sent = String::from_utf8(dialogue.reader.into_inner().sent).unwrap();
        assert_eq!(
            sent,
            "EHLO mail.local\r\nAUTH PLAIN AG1lQGljbG91ZC5jb20AcGFzcw==\r\nMAIL FROM:<me@icloud.com>\r\n\
             RCPT TO:<ana@example.org>\r\nRCPT TO:<bo@example.net>\r\nDATA\r\n\
             Subject: Hi\r\n\r\n..hidden\r\nok\r\n.\r\nQUIT\r\n"
        );
    }

    #[test]
    fn a_refused_recipient_stops_before_the_message() {
        let mut dialogue = server("250 hi\r\n235 ok\r\n250 ok\r\n550 5.1.1 no such user\r\n");
        let error = deliver(
            &mut dialogue,
            "me@icloud.com",
            "pass",
            &["nobody@example.org".into()],
            b"Subject: Hi\r\n\r\nbody\r\n",
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("550 5.1.1 no such user"),
            "{error:#}"
        );
        let sent = String::from_utf8(dialogue.reader.into_inner().sent).unwrap();
        assert!(!sent.contains("DATA"));
    }

    #[test]
    fn an_address_cant_carry_a_command() {
        let mut dialogue = server("");
        assert!(
            deliver(
                &mut dialogue,
                "me@icloud.com",
                "pass",
                &["a@b.org>\r\nRCPT TO:<c@d.org".into()],
                b"x",
            )
            .is_err()
        );
        assert!(dialogue.reader.into_inner().sent.is_empty());
    }
}
