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
    Question,
    Assign,
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
            Kind::Question => "?",
            Kind::Assign => "=",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Kind,
    pub text: String,
    pub num: f64,
    pub line: usize,
    pub col: usize,
}

impl Token {
    fn new(kind: Kind, text: &str, line: usize) -> Token {
        Token { kind, text: text.to_string(), num: 0.0, line, col: 1 }
    }

    fn at(mut self, col: usize) -> Token {
        self.col = col;
        self
    }
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    line_start: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer { src: src.as_bytes(), pos: 0, line: 1, line_start: 0 }
    }

    fn skip(&mut self) {
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == b'\n' {
                self.line += 1;
                self.pos += 1;
                self.line_start = self.pos;
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

    fn col(&self) -> usize {
        self.pos - self.line_start + 1
    }

    pub fn next_token(&mut self) -> Result<Token, String> {
        self.skip();
        let col = self.col();
        if self.pos >= self.src.len() {
            return Ok(Token::new(Kind::Eof, "", self.line).at(col));
        }
        let c = self.src[self.pos];
        if is_ident_start(c) {
            let start = self.pos;
            while self.pos < self.src.len() && is_ident(self.src[self.pos]) {
                self.pos += 1;
            }
            return Ok(Token::new(Kind::Ident, self.slice(start), self.line).at(col));
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
                .map_err(|_| format!("line {}:{}: bad number {:?}", self.line, col, text))?;
            let mut t = Token::new(Kind::Num, &text, self.line);
            t.num = num;
            return Ok(t.at(col));
        }
        if c == b'"' || c == b'\'' {
            return self.string(c).map(|t| t.at(col));
        }
        if c == b'=' && self.src.get(self.pos + 1) == Some(&b'>') {
            self.pos += 2;
            return Ok(Token::new(Kind::Arrow, "=>", self.line).at(col));
        }
        if c == b'=' && self.src.get(self.pos + 1) == Some(&b'=') {
            self.pos += 2;
            return Ok(Token::new(Kind::Eq, "==", self.line).at(col));
        }
        if c == b'!' && self.src.get(self.pos + 1) == Some(&b'=') {
            self.pos += 2;
            return Ok(Token::new(Kind::Ne, "!=", self.line).at(col));
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
            return Ok(Token::new(kind, kind.name(), self.line).at(col));
        }
        let kind = match c {
            b'+' => Kind::Plus,
            b'-' => Kind::Minus,
            b'*' => Kind::Star,
            b'/' => Kind::Slash,
            b'?' => Kind::Question,
            b'=' => Kind::Assign,
            b'(' => Kind::LParen,
            b')' => Kind::RParen,
            b'{' => Kind::LBrace,
            b'}' => Kind::RBrace,
            b'[' => Kind::LBrack,
            b']' => Kind::RBrack,
            b'.' => Kind::Dot,
            b',' => Kind::Comma,
            b':' => Kind::Colon,
            _ => {
                return Err(format!(
                    "line {}:{}: unexpected character {:?}",
                    self.line, col, c as char
                ))
            }
        };
        self.pos += 1;
        Ok(Token::new(kind, &(c as char).to_string(), self.line).at(col))
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
        let col = self.col();
        if self.pos >= self.src.len() || self.src[self.pos] != b'/' {
            return Err(format!("line {}:{}: expected path starting with /", self.line, col));
        }
        let start = self.pos;
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => break,
                _ => self.pos += 1,
            }
        }
        Ok(Token::new(Kind::Path, self.slice(start), self.line).at(col))
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
