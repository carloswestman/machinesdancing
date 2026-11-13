//! Just enough DNS to answer a stub resolver: one question per query, and
//! answers barre makes up itself. Nothing is ever forwarded upstream.

use std::net::Ipv4Addr;

pub const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
const HEADER: usize = 12;
/// Answers live this long, in seconds.
const TTL: u32 = 30;

const FORMERR: u8 = 1;
const SERVFAIL: u8 = 2;
const NXDOMAIN: u8 = 3;
const NOTIMP: u8 = 4;

#[derive(Debug, PartialEq, Eq)]
pub struct Query<'a> {
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
    /// The whole query, which the answer echoes.
    bytes: &'a [u8],
    /// Where the question ends.
    end: usize,
}

impl Query<'_> {
    /// Whether it asks for an IPv4 address.
    pub fn wants_a(&self) -> bool {
        self.qtype == TYPE_A && self.qclass == CLASS_IN
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// One `A` record.
    A(Ipv4Addr),
    /// The name exists, but has no records of the type asked for.
    Empty,
    NxDomain,
    ServFail,
}

/// Parses a query. `Err` carries the response to send instead, if the
/// query was long enough to have an id.
pub fn parse(bytes: &[u8]) -> Result<Query<'_>, Option<Vec<u8>>> {
    if bytes.len() < HEADER {
        return Err(None);
    }
    let flags = u16::from_be_bytes([bytes[2], bytes[3]]);
    let is_response = flags & 0x8000 != 0;
    let opcode = (flags >> 11) & 0xf;
    let count = |i: usize| u16::from_be_bytes([bytes[i], bytes[i + 1]]);
    if is_response {
        return Err(None);
    }
    if opcode != 0 {
        return Err(Some(error(bytes, NOTIMP)));
    }
    if count(4) != 1 {
        return Err(Some(error(bytes, FORMERR)));
    }
    let mut pos = HEADER;
    let mut labels = Vec::new();
    loop {
        let len = *bytes.get(pos).ok_or_else(|| Some(error(bytes, FORMERR)))? as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        // Compression has no place in a question.
        if len > 63 {
            return Err(Some(error(bytes, FORMERR)));
        }
        let label = bytes
            .get(pos..pos + len)
            .ok_or_else(|| Some(error(bytes, FORMERR)))?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        pos += len;
    }
    let fixed = bytes
        .get(pos..pos + 4)
        .ok_or_else(|| Some(error(bytes, FORMERR)))?;
    Ok(Query {
        name: labels.join("."),
        qtype: u16::from_be_bytes([fixed[0], fixed[1]]),
        qclass: u16::from_be_bytes([fixed[2], fixed[3]]),
        bytes,
        end: pos + 4,
    })
}

/// The response to `query`.
pub fn respond(query: &Query, answer: Answer) -> Vec<u8> {
    let rcode = match answer {
        Answer::A(_) | Answer::Empty => 0,
        Answer::NxDomain => NXDOMAIN,
        Answer::ServFail => SERVFAIL,
    };
    let mut out = header(query.bytes, rcode, 1, matches!(answer, Answer::A(_)) as u16);
    out.extend_from_slice(&query.bytes[HEADER..query.end]);
    if let Answer::A(ip) = answer {
        out.extend_from_slice(&[0xc0, HEADER as u8]); // the question's name
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&TTL.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&ip.octets());
    }
    out
}

fn error(query: &[u8], rcode: u8) -> Vec<u8> {
    header(query, rcode, 0, 0)
}

/// A response header for `query`: its id, opcode and recursion-desired bit,
/// authoritative, recursion available.
fn header(query: &[u8], rcode: u8, questions: u16, answers: u16) -> Vec<u8> {
    let flags = u16::from_be_bytes([query[2], query[3]]);
    let flags = 0x8000 | (flags & 0x7900) | 0x0400 | 0x0080 | rcode as u16;
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&query[0..2]);
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&questions.to_be_bytes());
    out.extend_from_slice(&answers.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A query as a stub resolver sends it, with an EDNS record after the
    /// question.
    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q.extend_from_slice(&[0, 0, 41, 4, 208, 0, 0, 0, 0, 0, 0]);
        q
    }

    #[test]
    fn a_query_parses() {
        let bytes = query("GitHub.com", TYPE_A);
        let q = parse(&bytes).unwrap();
        assert_eq!(q.name, "GitHub.com");
        assert!(q.wants_a());
        assert!(!parse(&query("github.com", 28)).unwrap().wants_a());
    }

    #[test]
    fn an_a_answer_echoes_the_question() {
        let bytes = query("github.com", TYPE_A);
        let q = parse(&bytes).unwrap();
        let r = respond(&q, Answer::A(Ipv4Addr::new(198, 18, 0, 1)));
        assert_eq!(&r[0..2], &[0x12, 0x34], "id");
        assert_eq!(r[2] & 0x80, 0x80, "a response");
        assert_eq!(r[2] & 0x01, 0x01, "recursion desired, echoed");
        assert_eq!(r[3] & 0x0f, 0, "no error");
        assert_eq!(&r[4..12], &[0, 1, 0, 1, 0, 0, 0, 0]);
        assert_eq!(&r[12..12 + 16], &bytes[12..12 + 16], "the question");
        assert_eq!(&r[r.len() - 4..], &[198, 18, 0, 1]);
        assert_eq!(r.len(), 12 + 16 + 16);
    }

    #[test]
    fn refusals_have_no_answers() {
        let bytes = query("example.com", TYPE_A);
        let q = parse(&bytes).unwrap();
        let r = respond(&q, Answer::NxDomain);
        assert_eq!(r[3] & 0x0f, NXDOMAIN);
        assert_eq!(&r[6..8], &[0, 0]);
        let r = respond(&q, Answer::Empty);
        assert_eq!((r[3] & 0x0f, &r[6..8]), (0, &[0u8, 0][..]));
    }

    #[test]
    fn malformed_queries_get_errors_or_nothing() {
        assert_eq!(parse(&[0; 5]), Err(None));
        let mut response = query("github.com", TYPE_A);
        response[2] |= 0x80;
        assert_eq!(parse(&response), Err(None), "responses are ignored");
        let mut truncated = query("github.com", TYPE_A);
        truncated.truncate(16);
        let err = parse(&truncated).unwrap_err().unwrap();
        assert_eq!(err[3] & 0x0f, FORMERR);
        let mut compressed = query("github.com", TYPE_A);
        compressed[12] = 0xc0;
        assert_eq!(parse(&compressed).unwrap_err().unwrap()[3] & 0x0f, FORMERR);
        let mut two = query("github.com", TYPE_A);
        two[5] = 2;
        assert_eq!(parse(&two).unwrap_err().unwrap()[3] & 0x0f, FORMERR);
    }
}
