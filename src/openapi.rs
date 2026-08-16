use crate::parser::{Program, Route};
use crate::value::{write_i64, write_string};
use std::collections::BTreeMap;

pub fn document(prog: &Program, title: &str, version: &str) -> Vec<u8> {
    let mut paths: BTreeMap<String, Vec<&Route>> = BTreeMap::new();
    for r in &prog.routes {
        paths.entry(openapi_path(&r.pattern)).or_default().push(r);
    }
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(b"{\"openapi\":\"3.0.3\",\"info\":{\"title\":");
    write_string(&mut out, title);
    out.extend_from_slice(b",\"version\":");
    write_string(&mut out, version);
    out.extend_from_slice(b"},\"paths\":{");
    for (i, (path, routes)) in paths.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_string(&mut out, path);
        out.extend_from_slice(b":{");
        for (j, r) in routes.iter().enumerate() {
            if j > 0 {
                out.push(b',');
            }
            write_string(&mut out, &r.method.name().to_lowercase());
            out.push(b':');
            operation(&mut out, r);
        }
        out.push(b'}');
    }
    out.extend_from_slice(b"}}");
    out
}

fn operation(out: &mut Vec<u8>, r: &Route) {
    out.extend_from_slice(b"{\"operationId\":");
    write_string(out, &operation_id(r));
    parameters(out, r);
    if r.uses_body {
        out.extend_from_slice(
            b",\"requestBody\":{\"required\":true,\"content\":{\"application/json\":{\"schema\":{\"type\":\"object\"}}}}",
        );
    }
    out.extend_from_slice(b",\"responses\":{");
    response(out, r.status, "ok", r.const_ctype);
    for extra in error_codes(r) {
        out.push(b',');
        let why = match &r.guard_msg {
            Some(reason) if extra == r.guard_status => reason.as_str(),
            _ => "error",
        };
        response(out, extra, why, crate::http::JSON);
    }
    out.extend_from_slice(b"}}");
}

fn response(out: &mut Vec<u8>, code: u16, description: &str, ctype: crate::http::Ctype) {
    let mut key = Vec::new();
    write_i64(&mut key, code as i64);
    write_string(out, std::str::from_utf8(&key).unwrap_or("200"));
    out.extend_from_slice(b":{\"description\":");
    write_string(out, description);
    if code != 204 && code != 304 {
        out.extend_from_slice(b",\"content\":{");
        write_string(out, ctype.split(';').next().unwrap_or(ctype));
        out.extend_from_slice(b":{\"schema\":{}}}");
    }
    out.push(b'}');
}

fn error_codes(r: &Route) -> Vec<u16> {
    let mut codes = Vec::new();
    if r.guard.is_some() {
        codes.push(r.guard_status);
    }
    if r.uses_body {
        codes.push(400);
    }
    if !r.params.is_empty() || r.pattern.contains(':') {
        codes.push(404);
    }
    codes.sort_unstable();
    codes.dedup();
    codes.retain(|c| *c != r.status);
    codes
}

fn parameters(out: &mut Vec<u8>, r: &Route) {
    if r.params.is_empty()
        && r.query_fields.is_empty()
        && r.header_fields.is_empty()
        && r.cookie_fields.is_empty()
    {
        return;
    }
    out.extend_from_slice(b",\"parameters\":[");
    let mut first = true;
    for name in &r.params {
        parameter(out, &mut first, name, "path", true);
    }
    for name in &r.query_fields {
        parameter(out, &mut first, name, "query", false);
    }
    for name in &r.header_fields {
        parameter(out, &mut first, &name.replace('_', "-"), "header", false);
    }
    for name in &r.cookie_fields {
        parameter(out, &mut first, name, "cookie", false);
    }
    out.push(b']');
}

fn parameter(out: &mut Vec<u8>, first: &mut bool, name: &str, place: &str, required: bool) {
    if !*first {
        out.push(b',');
    }
    *first = false;
    out.extend_from_slice(b"{\"name\":");
    write_string(out, name);
    out.extend_from_slice(b",\"in\":");
    write_string(out, place);
    out.extend_from_slice(if required {
        b",\"required\":true".as_slice()
    } else {
        b",\"required\":false".as_slice()
    });
    out.extend_from_slice(b",\"schema\":{\"type\":\"string\"}}");
}

fn operation_id(r: &Route) -> String {
    let mut id = r.method.name().to_lowercase();
    for seg in r.pattern.trim_matches('/').split('/') {
        if seg.is_empty() {
            continue;
        }
        id.push('_');
        id.push_str(&seg.replace(':', "by_"));
    }
    id
}

fn openapi_path(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 2);
    for seg in pattern.trim_end_matches('/').split('/') {
        if let Some(name) = seg.strip_prefix(':') {
            out.push('/');
            out.push('{');
            out.push_str(name);
            out.push('}');
        } else if !seg.is_empty() {
            out.push('/');
            out.push_str(seg);
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}
