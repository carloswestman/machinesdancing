//! Reading the name a connection asks for from its first bytes, without
//! changing them: the server name (SNI) in a TLS ClientHello, or the `Host`
//! header of a plain HTTP request.

/// What the first bytes of a connection say so far.
#[derive(Debug, PartialEq, Eq)]
pub enum Peek {
    /// Not enough bytes yet.
    Incomplete,
    /// The name it asks for, as sent.
    Name(String),
    /// A complete message that names nothing.
    NoName,
    /// Not the protocol expected.
    Invalid,
}

/// The server name in a TLS ClientHello, which may span several records.
pub fn sni(buf: &[u8]) -> Peek {
    const HANDSHAKE: u8 = 22;
    const CLIENT_HELLO: u8 = 1;
    let mut handshake = Vec::new();
    let mut pos = 0;
    while let Some(header) = buf.get(pos..pos + 5) {
        if header[0] != HANDSHAKE {
            return Peek::Invalid;
        }
        let len = u16::from_be_bytes([header[3], header[4]]) as usize;
        let Some(body) = buf.get(pos + 5..pos + 5 + len) else {
            break;
        };
        handshake.extend_from_slice(body);
        pos += 5 + len;
    }
    if handshake.len() < 4 {
        return Peek::Incomplete;
    }
    if handshake[0] != CLIENT_HELLO {
        return Peek::Invalid;
    }
    let len = u32::from_be_bytes([0, handshake[1], handshake[2], handshake[3]]) as usize;
    match handshake.get(4..4 + len) {
        Some(hello) => client_hello_sni(hello).unwrap_or(Peek::Invalid),
        None => Peek::Incomplete,
    }
}

/// The SNI in a ClientHello's body. None if it is malformed.
fn client_hello_sni(hello: &[u8]) -> Option<Peek> {
    const SERVER_NAME: u16 = 0;
    const HOST_NAME: u8 = 0;
    let mut r = Reader(hello);
    r.take(2 + 32)?; // version, random
    let session = r.u8()? as usize;
    r.take(session)?;
    let suites = r.u16()? as usize;
    r.take(suites)?;
    let compression = r.u8()? as usize;
    r.take(compression)?;
    if r.0.is_empty() {
        return Some(Peek::NoName); // no extensions at all
    }
    let len = r.u16()? as usize;
    let mut exts = Reader(r.take(len)?);
    while !exts.0.is_empty() {
        let kind = exts.u16()?;
        let len = exts.u16()? as usize;
        let data = exts.take(len)?;
        if kind != SERVER_NAME {
            continue;
        }
        let mut list = Reader(data);
        let len = list.u16()? as usize;
        let mut names = Reader(list.take(len)?);
        while !names.0.is_empty() {
            let kind = names.u8()?;
            let len = names.u16()? as usize;
            let name = names.take(len)?;
            if kind == HOST_NAME {
                return Some(Peek::Name(String::from_utf8(name.to_vec()).ok()?));
            }
        }
        return Some(Peek::NoName);
    }
    Some(Peek::NoName)
}

/// The `Host` header of an HTTP/1 request, without a port.
pub fn http_host(buf: &[u8]) -> Peek {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Peek::Incomplete;
    };
    let Ok(head) = std::str::from_utf8(&buf[..end]) else {
        return Peek::Invalid;
    };
    let mut lines = head.split("\r\n");
    let request = lines.next().unwrap_or_default();
    if !request.ends_with(" HTTP/1.1") && !request.ends_with(" HTTP/1.0") {
        return Peek::Invalid;
    }
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Peek::Invalid;
        };
        if name.eq_ignore_ascii_case("host") {
            let value = value.trim();
            let host = match value.rsplit_once(':') {
                Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
                _ => value,
            };
            return Peek::Name(host.to_string());
        }
    }
    Peek::NoName
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.0.len() {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ClientHello with the given extensions, in one TLS record.
    fn hello(extensions: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut body = vec![3, 3];
        body.extend_from_slice(&[7; 32]); // random
        body.push(32);
        body.extend_from_slice(&[9; 32]); // session id
        body.extend_from_slice(&[0, 4, 0x13, 0x01, 0x13, 0x02]); // suites
        body.extend_from_slice(&[1, 0]); // compression
        let mut exts = Vec::new();
        for (kind, data) in extensions {
            exts.extend_from_slice(&kind.to_be_bytes());
            exts.extend_from_slice(&(data.len() as u16).to_be_bytes());
            exts.extend_from_slice(data);
        }
        body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        body.extend_from_slice(&exts);
        let mut handshake = vec![1];
        handshake.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        handshake.extend_from_slice(&body);
        record(&handshake)
    }

    fn record(data: &[u8]) -> Vec<u8> {
        let mut r = vec![22, 3, 1];
        r.extend_from_slice(&(data.len() as u16).to_be_bytes());
        r.extend_from_slice(data);
        r
    }

    fn server_name(name: &str) -> (u16, Vec<u8>) {
        let mut entry = vec![0];
        entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
        entry.extend_from_slice(name.as_bytes());
        let mut list = (entry.len() as u16).to_be_bytes().to_vec();
        list.extend_from_slice(&entry);
        (0, list)
    }

    #[test]
    fn the_server_name_is_read() {
        let buf = hello(&[(10, vec![0, 2, 0, 29]), server_name("github.com")]);
        assert_eq!(sni(&buf), Peek::Name("github.com".into()));
    }

    #[test]
    fn a_hello_split_across_reads_is_incomplete_until_whole() {
        let buf = hello(&[server_name("github.com")]);
        for cut in [0, 3, 5, 9, buf.len() - 1] {
            assert_eq!(sni(&buf[..cut]), Peek::Incomplete, "cut at {cut}");
        }
        assert_eq!(sni(&buf), Peek::Name("github.com".into()));
    }

    #[test]
    fn a_hello_split_across_records_is_read() {
        let whole = hello(&[server_name("github.com")]);
        let handshake = &whole[5..];
        let (a, b) = handshake.split_at(20);
        let mut buf = record(a);
        buf.extend_from_slice(&record(b));
        assert_eq!(sni(&buf), Peek::Name("github.com".into()));
    }

    #[test]
    fn a_hello_without_a_name_names_nothing() {
        assert_eq!(sni(&hello(&[(10, vec![0, 2, 0, 29])])), Peek::NoName);
        assert_eq!(sni(&hello(&[])), Peek::NoName);
    }

    #[test]
    fn other_bytes_are_invalid() {
        assert_eq!(sni(b"GET / HTTP/1.1\r\n\r\n"), Peek::Invalid);
        assert_eq!(sni(b"SSH-2.0-OpenSSH_9.9\r\n"), Peek::Invalid);
        let mut bad = hello(&[server_name("github.com")]);
        let len = bad.len();
        bad[len - 12] = 0xff; // a server name length past the end
        assert_eq!(sni(&bad), Peek::Invalid);
    }

    #[test]
    fn the_host_header_is_read() {
        assert_eq!(
            http_host(b"GET / HTTP/1.1\r\nUser-Agent: x\r\nHOST: Example.com:80\r\n\r\n"),
            Peek::Name("Example.com".into())
        );
        assert_eq!(http_host(b"GET / HTTP/1.1\r\nHost: exa"), Peek::Incomplete);
        assert_eq!(http_host(b"GET / HTTP/1.0\r\n\r\n"), Peek::NoName);
        assert_eq!(http_host(b"\x16\x03\x01\x00\x05\r\n\r\n"), Peek::Invalid);
    }
}
