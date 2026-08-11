use crate::parser::{Ctx, Method, Route, MAX_PARAMS, N_METHODS};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)]
pub struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf29ce484222325 } else { self.0 };
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        self.0 = h;
    }
}

type FnvMap<K, V> = HashMap<K, V, BuildHasherDefault<Fnv>>;

#[derive(Default)]
struct Node {
    static_kids: Vec<(String, usize)>,
    param: Option<usize>,
    route: Option<usize>,
}

pub struct Router {
    exact: Vec<FnvMap<String, usize>>,
    roots: Vec<Option<usize>>,
    nodes: Vec<Node>,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Router {
        Router {
            exact: (0..N_METHODS).map(|_| FnvMap::default()).collect(),
            roots: vec![None; N_METHODS],
            nodes: Vec::new(),
        }
    }

    pub fn build(routes: &[Route]) -> Result<Router, String> {
        let mut r = Router::new();
        for (i, rt) in routes.iter().enumerate() {
            r.add(rt, i)?;
        }
        Ok(r)
    }

    fn node(&mut self) -> usize {
        self.nodes.push(Node::default());
        self.nodes.len() - 1
    }

    fn add(&mut self, rt: &Route, idx: usize) -> Result<(), String> {
        let mi = rt.method.index();
        let path = normalize(&rt.pattern).to_string();
        let dup = || format!("duplicate route {} {}", rt.method.name(), rt.pattern);
        if !path.contains(':') {
            if self.exact[mi].insert(path, idx).is_some() {
                return Err(dup());
            }
            return Ok(());
        }
        let mut cur = match self.roots[mi] {
            Some(n) => n,
            None => {
                let n = self.node();
                self.roots[mi] = Some(n);
                n
            }
        };
        for seg in path.trim_matches('/').split('/') {
            if seg.starts_with(':') {
                cur = match self.nodes[cur].param {
                    Some(n) => n,
                    None => {
                        let n = self.node();
                        self.nodes[cur].param = Some(n);
                        n
                    }
                };
                continue;
            }
            cur = match self.nodes[cur].static_kids.iter().find(|(s, _)| s == seg) {
                Some(&(_, n)) => n,
                None => {
                    let n = self.node();
                    self.nodes[cur].static_kids.push((seg.to_string(), n));
                    n
                }
            };
        }
        if self.nodes[cur].route.is_some() {
            return Err(dup());
        }
        self.nodes[cur].route = Some(idx);
        Ok(())
    }

    pub fn lookup<'a>(&self, m: Method, path: &'a str, c: &mut Ctx<'a>) -> Option<usize> {
        self.lookup_index(m.index(), path, Some(c))
    }

    pub fn allows(&self, path: &str) -> bool {
        (0..N_METHODS).any(|i| self.lookup_index(i, path, None).is_some())
    }

    fn lookup_index<'a>(
        &self,
        mi: usize,
        path: &'a str,
        mut c: Option<&mut Ctx<'a>>,
    ) -> Option<usize> {
        let p = normalize(path);
        if let Some(&idx) = self.exact[mi].get(p) {
            if let Some(c) = c.as_deref_mut() {
                c.nparams = 0;
            }
            return Some(idx);
        }
        let mut cur = self.roots[mi]?;
        let mut n = 0;
        let mut rest = p.strip_prefix('/').unwrap_or(p);
        while !rest.is_empty() {
            let (seg, next) = match rest.find('/') {
                Some(j) => (&rest[..j], &rest[j + 1..]),
                None => (rest, ""),
            };
            rest = next;
            if let Some(&(_, nx)) = self.nodes[cur].static_kids.iter().find(|(s, _)| s == seg) {
                cur = nx;
                continue;
            }
            match self.nodes[cur].param {
                Some(nx) if !seg.is_empty() => {
                    if n < MAX_PARAMS {
                        if let Some(c) = c.as_deref_mut() {
                            c.params[n] = seg;
                        }
                    }
                    n += 1;
                    cur = nx;
                }
                _ => return None,
            }
        }
        let idx = self.nodes[cur].route?;
        if let Some(c) = c {
            c.nparams = n.min(MAX_PARAMS);
        }
        Some(idx)
    }
}

fn normalize(p: &str) -> &str {
    if p.is_empty() {
        return "/";
    }
    if p.len() > 1 && p.ends_with('/') {
        return &p[..p.len() - 1];
    }
    p
}
