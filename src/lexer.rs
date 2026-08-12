#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Eof,
    Ident,
    Str,
    Num,
    Path,
    Arrow,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Dot,
    Comma,
    Colon,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Eof => "end of file",
            Kind::Ident => "identifier",
            Kind::Str => "string",
            Kind::Num => "number",
            Kind::Path => "path",
            Kind::Arrow => "=>",
            Kind::LParen => "(",
            Kind::RParen => ")",
            Kind::LBrace => "{",
            Kind::RBrace => "}",
            Kind::LBrack => "[",
            Kind::RBrack => "]",
            Kind::Dot => ".",
            Kind::Comma => ",",
            Kind::Colon => ":",
            Kind::Eq => "==",
            Kind::Ne => "!=",
            Kind::Lt => "<",
            Kind::Gt => ">",
            Kind::Le => "<=",
            Kind::Ge => ">=",
            Kind::Plus => "+",
            Kind::Minus => "-",
            Kind::Star => "*",
            Kind::Slash => "/",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Kind,
    pub text: String,
    pub num: f64,
    pub line: usize,
}

impl Token {
    fn new(kind: Kind, text: &str, line: usize) -> Token {
        Token { kind, text: text.to_string(), num: 0.0, line }
    }
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer { src: src.as_bytes(), pos: 0, line: 1 }
    }

    fn skip(&mut self) {
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == b'\n' {
                self.line += 1;
                self.pos += 1;
            } else if c == b' ' || c == b'\t' || c == b'\r' {
                self.pos += 1;
            } else if c == b'#' || (c == b'/' && self.src.get(self.pos + 1) == Some(&b'/')) {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                return;
            }
        }
    }

    pub fn next_token(&mut self) -> Result<Token, String> {
        self.skip();
        if self.pos >= self.src.len() {
            return Ok(Token::new(Kind::Eof, "", self.line));
        }
        let c = self.src[self.pos];
        if is_ident_start(c) {
            let start = self.pos;
            while self.pos < self.src.len() && is_ident(self.src[self.pos]) {
                self.pos += 1;
            }
            return Ok(Token::new(Kind::Ident, self.slice(start), self.line));
        }
        if c.is_ascii_digit()
            || (c == b'-' && self.src.get(self.pos + 1).is_some_and(|n| n.is_ascii_digit()))
        {
            let start = self.pos;
            self.pos += 1;
            while self.pos < self.src.len()
                && (self.src[self.pos].is_ascii_digit() || self.src[self.pos] == b'.')
            {
                self.pos += 1;
            }
            let text = self.slice(start).to_string();
            let num = text
                .parse::<f64>()
                .map_err(|_| format!("line {}: bad number {:?}", self.line, text))?;
            let mut t = Token::new(Kind::Num, &text, self.line);
            t.num = num;
            return Ok(t);
        }
        if c == b'"' || c == b'\'' {
            return self.string(c);
        }
        if c == b'=' && self.src.get(self.pos + 1) == Some(&b'>') {
            self.pos += 2;
            return Ok(Token::new(Kind::Arrow, "=>", self.line));
        }
        if c == b'=' && self.src.get(self.pos + 1) == Some(&b'=') {
            self.pos += 2;
            return Ok(Token::new(Kind::Eq, "==", self.line));
        }
        if c == b'!' && self.src.get(self.pos + 1) == Some(&b'=') {
            self.pos += 2;
            return Ok(Token::new(Kind::Ne, "!=", self.line));
        }
        if c == b'<' || c == b'>' {
            let eq = self.src.get(self.pos + 1) == Some(&b'=');
            self.pos += if eq { 2 } else { 1 };
            let kind = match (c, eq) {
                (b'<', false) => Kind::Lt,
                (b'<', true) => Kind::Le,
                (b'>', false) => Kind::Gt,
                _ => Kind::Ge,
            };
            return Ok(Token::new(kind, kind.name(), self.line));
        }
        let kind = match c {
            b'+' => Kind::Plus,
            b'-' => Kind::Minus,
            b'*' => Kind::Star,
            b'/' => Kind::Slash,
            b'(' => Kind::LParen,
            b')' => Kind::RParen,
            b'{' => Kind::LBrace,
            b'}' => Kind::RBrace,
            b'[' => Kind::LBrack,
            b']' => Kind::RBrack,
            b'.' => Kind::Dot,
            b',' => Kind::Comma,
            b':' => Kind::Colon,
            _ => return Err(format!("line {}: unexpected character {:?}", self.line, c as char)),
        };
        self.pos += 1;
        Ok(Token::new(kind, &(c as char).to_string(), self.line))
    }

    fn slice(&self, start: usize) -> &str {
        std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("")
    }

    fn string(&mut self, quote: u8) -> Result<Token, String> {
        let line = self.line;
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == quote {
                self.pos += 1;
                return Ok(Token::new(Kind::Str, &out, line));
            }
            if c == b'\\' && self.pos + 1 < self.src.len() {
                self.pos += 1;
                match self.src[self.pos] {
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    other => out.push(other as char),
                }
                self.pos += 1;
                continue;
            }
            if c == b'\n' {
                return Err(format!("line {}: unterminated string", line));
            }
            let start = self.pos;
            let len = utf8_len(c);
            self.pos += len;
            let chunk = std::str::from_utf8(&self.src[start..self.pos.min(self.src.len())])
                .map_err(|_| format!("line {}: invalid utf-8 in string", line))?;
            out.push_str(chunk);
        }
        Err(format!("line {}: unterminated string", line))
    }

    pub fn path(&mut self) -> Result<Token, String> {
        self.skip();
        if self.pos >= self.src.len() || self.src[self.pos] != b'/' {
            return Err(format!("line {}: expected path starting with /", self.line));
        }
        let start = self.pos;
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => break,
                _ => self.pos += 1,
            }
        }
        Ok(Token::new(Kind::Path, self.slice(start), self.line))
    }
}

fn utf8_len(c: u8) -> usize {
    match c {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}
