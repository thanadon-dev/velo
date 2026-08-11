use std::time::{SystemTime, UNIX_EPOCH};

const DAYS: [&[u8; 3]; 7] = [b"Thu", b"Fri", b"Sat", b"Sun", b"Mon", b"Tue", b"Wed"];
const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

pub fn header(now: SystemTime) -> Vec<u8> {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut out = Vec::with_capacity(37);
    out.extend_from_slice(b"Date: ");
    write_imf(&mut out, secs);
    out.extend_from_slice(b"\r\n");
    out
}

pub fn write_imf(out: &mut Vec<u8>, secs: u64) {
    let days = (secs / 86400) as i64;
    let tod = secs % 86400;
    out.extend_from_slice(DAYS[(days % 7) as usize]);
    out.extend_from_slice(b", ");
    let (y, m, d) = civil_from_days(days);
    two(out, d as u64);
    out.push(b' ');
    out.extend_from_slice(MONTHS[(m - 1) as usize]);
    out.push(b' ');
    four(out, y as u64);
    out.push(b' ');
    two(out, tod / 3600);
    out.push(b':');
    two(out, (tod / 60) % 60);
    out.push(b':');
    two(out, tod % 60);
    out.extend_from_slice(b" GMT");
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn two(out: &mut Vec<u8>, n: u64) {
    out.push(b'0' + (n / 10 % 10) as u8);
    out.push(b'0' + (n % 10) as u8);
}

fn four(out: &mut Vec<u8>, n: u64) {
    out.push(b'0' + (n / 1000 % 10) as u8);
    out.push(b'0' + (n / 100 % 10) as u8);
    out.push(b'0' + (n / 10 % 10) as u8);
    out.push(b'0' + (n % 10) as u8);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn known_timestamps() {
        let at = |s: u64| {
            let mut out = Vec::new();
            write_imf(&mut out, s);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(at(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(at(1_000_000_000), "Sun, 09 Sep 2001 01:46:40 GMT");
        assert_eq!(at(1_755_000_000), "Tue, 12 Aug 2025 12:00:00 GMT");
        assert_eq!(at(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
        assert_eq!(
            String::from_utf8(header(UNIX_EPOCH + Duration::from_secs(1_755_000_000))).unwrap(),
            "Date: Tue, 12 Aug 2025 12:00:00 GMT\r\n"
        );
    }
}
