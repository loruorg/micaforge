//! When does a server's certificate expire?
//!
//! The TLS stack hands over the end-entity certificate as DER bytes, and the
//! one fact the uptime check needs from it is `notAfter`. A full X.509 parser
//! is a large dependency to read one field, so this walks the DER structure
//! as far as the validity period and no further:
//!
//! ```text
//! Certificate     ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
//! TBSCertificate  ::= SEQUENCE {
//!     version         [0] EXPLICIT INTEGER OPTIONAL,
//!     serialNumber    INTEGER,
//!     signature       AlgorithmIdentifier,   -- SEQUENCE
//!     issuer          Name,                  -- SEQUENCE
//!     validity        SEQUENCE { notBefore Time, notAfter Time },
//!     ... }
//! Time            ::= UTCTime | GeneralizedTime
//! ```
//!
//! Nothing here verifies the certificate: rustls has already done that, or
//! refused the connection. A structure this reader does not understand is
//! `None`, which the check reports as "expiry unknown" rather than as a
//! failure, because a parser's blind spot is not the site's outage.

use chrono::{DateTime, NaiveDate, Utc};

/// DER tag of a SEQUENCE.
const SEQUENCE: u8 = 0x30;
/// DER tag of an INTEGER.
const INTEGER: u8 = 0x02;
/// The context-specific, constructed `[0]` that wraps the version.
const VERSION: u8 = 0xA0;
/// DER tag of a UTCTime.
const UTC_TIME: u8 = 0x17;
/// DER tag of a GeneralizedTime.
const GENERALIZED_TIME: u8 = 0x18;

/// The `notAfter` of a DER-encoded X.509 certificate.
pub fn not_after(der: &[u8]) -> Option<DateTime<Utc>> {
    let (tag, certificate, _) = read_tlv(der)?;
    if tag != SEQUENCE {
        return None;
    }
    let (tag, tbs, _) = read_tlv(certificate)?;
    if tag != SEQUENCE {
        return None;
    }

    let mut rest = tbs;
    let (mut tag, mut _value, mut after) = read_tlv(rest)?;
    if tag == VERSION {
        rest = after;
        (tag, _value, after) = read_tlv(rest)?;
    }
    if tag != INTEGER {
        return None;
    }
    // signature algorithm, issuer, validity.
    rest = after;
    let (tag, _, after) = read_tlv(rest)?;
    if tag != SEQUENCE {
        return None;
    }
    let (tag, _, after) = read_tlv(after)?;
    if tag != SEQUENCE {
        return None;
    }
    let (tag, validity, _) = read_tlv(after)?;
    if tag != SEQUENCE {
        return None;
    }

    let (_, _, after) = read_tlv(validity)?;
    let (tag, raw, _) = read_tlv(after)?;
    parse_time(tag, raw)
}

/// Whole days from `now` until `expires`, rounded down, negative once passed.
pub fn days_left(expires: DateTime<Utc>, now: DateTime<Utc>) -> i32 {
    let seconds = (expires - now).num_seconds();
    let days = seconds.div_euclid(86_400);
    i32::try_from(days).unwrap_or(if days < 0 { i32::MIN } else { i32::MAX })
}

/// One tag-length-value: `(tag, value, rest)`.
///
/// Only the single-byte tags and the definite lengths DER allows are read: a
/// length of up to four bytes is far past any certificate, and an indefinite
/// length is BER, never DER.
fn read_tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let len = rest[..count]
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (len, &rest[count..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// A UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`).
///
/// RFC 5280 requires both to be in UTC with seconds and no fraction, and
/// gives the UTCTime century rule: `YY` below 50 is 20YY, otherwise 19YY.
fn parse_time(tag: u8, raw: &[u8]) -> Option<DateTime<Utc>> {
    let text = std::str::from_utf8(raw).ok()?;
    let digits = text.strip_suffix('Z')?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (year, rest) = match (tag, digits.len()) {
        (UTC_TIME, 12) => {
            let yy: i32 = digits[..2].parse().ok()?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &digits[2..])
        }
        (GENERALIZED_TIME, 14) => (digits[..4].parse().ok()?, &digits[4..]),
        _ => return None,
    };
    let field = |at: usize| rest[at..at + 2].parse::<u32>().ok();
    NaiveDate::from_ymd_opt(year, field(0)?, field(2)?)?
        .and_hms_opt(field(4)?, field(6)?, field(8)?)
        .map(|naive| naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// One DER element with a definite length.
    fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        match value.len() {
            n if n < 0x80 => out.push(n as u8),
            n if n <= 0xff => out.extend([0x81, n as u8]),
            n => out.extend([0x82, (n >> 8) as u8, n as u8]),
        }
        out.extend_from_slice(value);
        out
    }

    /// A certificate skeleton with the given validity times, optionally with
    /// the v3 version wrapper.
    fn certificate(not_before: Vec<u8>, not_after: Vec<u8>, v3: bool) -> Vec<u8> {
        let mut tbs = Vec::new();
        if v3 {
            tbs.extend(tlv(VERSION, &tlv(INTEGER, &[2])));
        }
        tbs.extend(tlv(INTEGER, &[0x01, 0x23, 0x45]));
        tbs.extend(tlv(SEQUENCE, &tlv(0x06, &[0x2a, 0x86, 0x48])));
        tbs.extend(tlv(SEQUENCE, &tlv(0x31, &[0x00])));
        tbs.extend(tlv(SEQUENCE, &[not_before, not_after].concat()));
        tbs.extend(tlv(SEQUENCE, &[]));

        let mut cert = tlv(SEQUENCE, &tbs);
        cert.extend(tlv(SEQUENCE, &[]));
        cert.extend(tlv(0x03, &[0x00, 0xAA]));
        tlv(SEQUENCE, &cert)
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn a_v3_certificate_with_utc_times_reads_its_expiry() {
        let der = certificate(
            tlv(UTC_TIME, b"260101000000Z"),
            tlv(UTC_TIME, b"270315120000Z"),
            true,
        );
        assert_eq!(
            not_after(&der),
            Some(Utc.with_ymd_and_hms(2027, 3, 15, 12, 0, 0).unwrap())
        );
    }

    #[test]
    fn generalized_time_and_the_utctime_century_rule() {
        let der = certificate(
            tlv(UTC_TIME, b"990101000000Z"),
            tlv(GENERALIZED_TIME, b"20510630235959Z"),
            false,
        );
        assert_eq!(
            not_after(&der),
            Some(Utc.with_ymd_and_hms(2051, 6, 30, 23, 59, 59).unwrap())
        );
        // A UTCTime of 49 is 2049; 50 is 1950.
        assert_eq!(
            parse_time(UTC_TIME, b"491231235959Z"),
            Some(Utc.with_ymd_and_hms(2049, 12, 31, 23, 59, 59).unwrap())
        );
        assert_eq!(
            parse_time(UTC_TIME, b"500101000000Z"),
            Some(Utc.with_ymd_and_hms(1950, 1, 1, 0, 0, 0).unwrap())
        );
    }

    #[test]
    fn a_real_certificate_from_openssl_reads_the_same_date_openssl_prints() {
        // `openssl req -x509 -newkey ec ... -days 400`, a v1 certificate
        // with long-form lengths, for which `openssl x509 -noout -dates`
        // prints `notAfter=Nov 12 18:21:09 2027 GMT`.
        let der = hex(concat!(
            "30820220308201c6020900d5cb7b6bab71ef12300a06082a8648ce3d040302301e311c301a",
            "06035504030c13757074696d652e6578616d706c652e74657374301e170d32363130303831",
            "38323130395a170d3237313131323138323130395a301e311c301a06035504030c13757074",
            "696d652e6578616d706c652e746573743082014b3082010306072a8648ce3d02013081f702",
            "0101302c06072a8648ce3d0101022100ffffffff0000000100000000000000000000000000",
            "ffffffffffffffffffffffff305b0420ffffffff0000000100000000000000000000000000",
            "fffffffffffffffffffffffc04205ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0",
            "f63bce3c3e27d2604b031500c49d360886e704936a6678e1139d26b7819f7e900441046b17",
            "d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f",
            "9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5022100ffffffff00000000ff",
            "ffffffffffffffbce6faada7179e84f3b9cac2fc63255102010103420004596e18ff9b525d",
            "2954d8f380a8da0f31623e974ed375d06d3cdb111fee131951cc4d5460da2fbe4aab8e5494",
            "1da7b3a9199d7a9745dede421cbd3373787511ae300a06082a8648ce3d0403020348003045",
            "022100f3a6922f099a30bf369f803ed16e98df71f6f6e78900060c244dd9c72f389ef20220",
            "23c3eae1b0de8ee7245554e69ea6be2ae2f1d335958e8d37626d377fc4121193",
        ));
        assert_eq!(
            not_after(&der),
            Some(Utc.with_ymd_and_hms(2027, 11, 12, 18, 21, 9).unwrap())
        );
    }

    #[test]
    fn anything_malformed_is_unknown_rather_than_a_guess() {
        assert_eq!(not_after(&[]), None);
        assert_eq!(not_after(&[0x30, 0x05, 0x01]), None, "truncated");
        assert_eq!(not_after(&tlv(INTEGER, &[1])), None, "not a sequence");
        // A local time without the `Z` is not what RFC 5280 allows.
        let der = certificate(
            tlv(UTC_TIME, b"260101000000Z"),
            tlv(UTC_TIME, b"2703151200+0100"),
            true,
        );
        assert_eq!(not_after(&der), None);
        // An impossible date.
        assert_eq!(parse_time(UTC_TIME, b"271332120000Z"), None);
        // Indefinite length is BER, not DER.
        assert_eq!(read_tlv(&[0x30, 0x80, 0x00, 0x00]), None);
    }

    #[test]
    fn days_left_rounds_down_and_goes_negative() {
        let now = Utc.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap();
        assert_eq!(days_left(now + chrono::Duration::hours(36), now), 1);
        assert_eq!(days_left(now + chrono::Duration::hours(23), now), 0);
        assert_eq!(days_left(now - chrono::Duration::hours(1), now), -1);
        assert_eq!(days_left(now - chrono::Duration::days(3), now), -3);
    }
}
