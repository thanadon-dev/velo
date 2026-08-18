use crate::value::{intern, Obj, Value};
use std::path::Path;

pub fn boundary_of(ctype: &str) -> Option<String> {
    let (kind, rest) = ctype.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in rest.split(';') {
        let (key, value) = param.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("boundary") {
            let value = value.trim().trim_matches('"');
            return (!value.is_empty() && value.len() <= 70).then(|| value.to_string());
        }
    }
    None
}

pub enum Refused {
    Malformed,
    NotText,
    NoStore,
}

pub fn parse(body: &[u8], boundary: &str, store: Option<&Path>) -> Result<Value, Refused> {
    let mut mark = Vec::with_capacity(boundary.len() + 4);
    mark.extend_from_slice(b"--");
    mark.extend_from_slice(boundary.as_bytes());
    let Some(first) = find(body, &mark) else { return Err(Refused::Malformed) };
    let mut at = first + mark.len();
    let mut fields: Obj = Vec::new();
    loop {
        match body.get(at..at + 2) {
            Some(b"--") => break,
            Some(b"\r\n") => at += 2,
            _ => return Err(Refused::Malformed),
        }
        let Some(gap) = find(&body[at..], b"\r\n\r\n") else { return Err(Refused::Malformed) };
        let head = &body[at..at + gap];
        at += gap + 4;
        let Some(end) = find(&body[at..], &mark) else { return Err(Refused::Malformed) };
        let content = &body[at..at + end.saturating_sub(2)];
        at += end + mark.len();
        let Some((name, filename)) = disposition(head) else { return Err(Refused::Malformed) };
        let value = match filename {
            None => match std::str::from_utf8(content) {
                Ok(text) => Value::str(text),
                Err(_) => return Err(Refused::NotText),
            },
            Some(filename) => {
                let Some(dir) = store else { return Err(Refused::NoStore) };
                kept(dir, &filename, part_type(head), content)?
            }
        };
        match fields.iter_mut().find(|(k, _)| **k == *name) {
            Some(slot) => slot.1 = value,
            None => fields.push((intern(&name), value)),
        }
    }
    Ok(Value::obj(fields))
}

fn kept(dir: &Path, filename: &str, ctype: String, content: &[u8]) -> Result<Value, Refused> {
    let mut at = dir.join(crate::ast::uuid());
    let suffix = extension(filename);
    if !suffix.is_empty() {
        at.set_extension(&suffix);
    }
    if std::fs::create_dir_all(dir).is_err() || std::fs::write(&at, content).is_err() {
        return Err(Refused::NoStore);
    }
    let leaf = at.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Value::obj(vec![
        (intern("name"), Value::str(filename)),
        (intern("type"), Value::str(&ctype)),
        (intern("size"), Value::Num(content.len() as f64)),
        (intern("file"), Value::str(&leaf)),
    ]))
}

fn extension(filename: &str) -> String {
    let Some((_, tail)) = filename.rsplit_once('.') else { return String::new() };
    let clean = tail.to_ascii_lowercase();
    let ok = !clean.is_empty()
        && clean.len() <= 8
        && clean.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    match ok {
        true => clean,
        false => String::new(),
    }
}

fn disposition(head: &[u8]) -> Option<(String, Option<String>)> {
    let text = std::str::from_utf8(head).ok()?;
    let line = text.split("\r\n").find(|l| {
        l.split_once(':').is_some_and(|(k, _)| k.eq_ignore_ascii_case("content-disposition"))
    })?;
    let name = quoted(line, "name")?;
    if name.is_empty() {
        return None;
    }
    Some((name, quoted(line, "filename")))
}

fn part_type(head: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(head) else { return String::new() };
    text.split("\r\n")
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-type").then(|| v.trim().to_string())
        })
        .unwrap_or_default()
}

fn quoted(line: &str, key: &str) -> Option<String> {
    let mut rest = line;
    while let Some(at) = rest.find(key) {
        let after = &rest[at + key.len()..];
        let before_ok = rest[..at].ends_with([' ', ';']);
        if before_ok {
            if let Some(value) = after.strip_prefix("=\"") {
                if let Some(end) = value.find('"') {
                    return Some(value[..end].to_string());
                }
            }
        }
        rest = after;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::{boundary_of, extension, parse, Refused};
    use crate::value::Value;

    fn body(boundary: &str, parts: &[(&str, Option<&str>, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, filename, content) in parts {
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            out.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"").as_bytes(),
            );
            if let Some(f) = filename {
                out.extend_from_slice(format!("; filename=\"{f}\"").as_bytes());
            }
            out.extend_from_slice(b"\r\n\r\n");
            out.extend_from_slice(content.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        out
    }

    #[test]
    fn a_boundary_is_read_only_from_a_multipart_content_type() {
        assert_eq!(boundary_of("multipart/form-data; boundary=abc"), Some("abc".into()));
        assert_eq!(
            boundary_of("multipart/form-data; charset=utf-8; boundary=\"a b\""),
            Some("a b".into())
        );
        assert_eq!(boundary_of("application/json"), None);
        assert_eq!(boundary_of("application/json; boundary=abc"), None);
        assert_eq!(boundary_of("multipart/mixed; boundary=abc"), None);
        assert_eq!(boundary_of("multipart/form-data"), None);
        assert_eq!(boundary_of("multipart/form-data; boundary="), None);
    }

    #[test]
    fn text_parts_become_fields_and_the_last_of_a_name_wins() {
        let raw = body("x", &[("a", None, "one"), ("b", None, "two"), ("a", None, "three")]);
        let v = parse(&raw, "x", None).ok().unwrap();
        assert_eq!(v.get("a").as_key(), "three");
        assert_eq!(v.get("b").as_key(), "two");
    }

    #[test]
    fn a_file_part_without_somewhere_to_put_it_is_refused_rather_than_dropped() {
        let raw = body("x", &[("f", Some("a.png"), "bytes")]);
        assert!(matches!(parse(&raw, "x", None), Err(Refused::NoStore)));
    }

    #[test]
    fn a_body_that_does_not_close_is_refused() {
        let mut raw = body("x", &[("a", None, "one")]);
        raw.truncate(raw.len() - 8);
        assert!(matches!(parse(&raw, "x", None), Err(Refused::Malformed)));
        assert!(matches!(parse(b"nothing here", "x", None), Err(Refused::Malformed)));
    }

    #[test]
    fn only_a_plain_short_extension_is_carried_over_from_a_client_filename() {
        assert_eq!(extension("photo.PNG"), "png");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("no-dot"), "");
        assert_eq!(extension("evil../../etc/passwd"), "");
        assert_eq!(extension("x.verylongextension"), "");
        assert_eq!(extension("x."), "");
    }

    #[test]
    fn a_part_that_is_not_text_and_is_not_a_file_is_refused() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"--x\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n");
        raw.extend_from_slice(&[0xff, 0xfe]);
        raw.extend_from_slice(b"\r\n--x--\r\n");
        assert!(matches!(parse(&raw, "x", None), Err(Refused::NotText)));
    }

    #[test]
    fn an_empty_form_is_an_empty_object() {
        let v = parse(b"--x--\r\n", "x", None).ok().unwrap();
        assert!(matches!(v, Value::Obj(_)));
        assert_eq!(v.get("anything").as_key(), "");
    }
}
