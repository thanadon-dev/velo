package velo

import (
	"fmt"
	"strconv"
	"strings"
)

type Kind uint8

const (
	tEOF Kind = iota
	tIdent
	tString
	tNumber
	tPath
	tArrow
	tLParen
	tRParen
	tLBrace
	tRBrace
	tLBrack
	tRBrack
	tDot
	tComma
	tColon
)

var kindNames = map[Kind]string{
	tEOF: "end of file", tIdent: "identifier", tString: "string", tNumber: "number",
	tPath: "path", tArrow: "=>", tLParen: "(", tRParen: ")", tLBrace: "{",
	tRBrace: "}", tLBrack: "[", tRBrack: "]", tDot: ".", tComma: ",", tColon: ":",
}

var singleTok = map[byte]Kind{'(': tLParen, ')': tRParen, '{': tLBrace, '}': tRBrace,
	'[': tLBrack, ']': tRBrack, '.': tDot, ',': tComma, ':': tColon}

type Token struct {
	Kind Kind
	Text string
	Num  float64
	Line int
}

type lexer struct {
	src  string
	pos  int
	line int
}

func newLexer(src string) *lexer {
	return &lexer{src: src, line: 1}
}

func (l *lexer) skip() {
	for l.pos < len(l.src) {
		c := l.src[l.pos]
		if c == '\n' {
			l.line++
			l.pos++
			continue
		}
		if c == ' ' || c == '\t' || c == '\r' {
			l.pos++
			continue
		}
		if c == '#' {
			for l.pos < len(l.src) && l.src[l.pos] != '\n' {
				l.pos++
			}
			continue
		}
		if c == '/' && l.pos+1 < len(l.src) && l.src[l.pos+1] == '/' {
			for l.pos < len(l.src) && l.src[l.pos] != '\n' {
				l.pos++
			}
			continue
		}
		return
	}
}

func (l *lexer) next() (Token, error) {
	l.skip()
	if l.pos >= len(l.src) {
		return Token{Kind: tEOF, Line: l.line}, nil
	}
	c := l.src[l.pos]
	switch {
	case isIdentStart(c):
		start := l.pos
		for l.pos < len(l.src) && isIdent(l.src[l.pos]) {
			l.pos++
		}
		return Token{Kind: tIdent, Text: l.src[start:l.pos], Line: l.line}, nil
	case c >= '0' && c <= '9' || c == '-':
		start := l.pos
		l.pos++
		for l.pos < len(l.src) && (l.src[l.pos] >= '0' && l.src[l.pos] <= '9' || l.src[l.pos] == '.') {
			l.pos++
		}
		f, err := strconv.ParseFloat(l.src[start:l.pos], 64)
		if err != nil {
			return Token{}, fmt.Errorf("line %d: bad number %q", l.line, l.src[start:l.pos])
		}
		return Token{Kind: tNumber, Text: l.src[start:l.pos], Num: f, Line: l.line}, nil
	case c == '"' || c == '\'':
		return l.str(c)
	case c == '=' && l.pos+1 < len(l.src) && l.src[l.pos+1] == '>':
		l.pos += 2
		return Token{Kind: tArrow, Text: "=>", Line: l.line}, nil
	}
	if k, ok := singleTok[c]; ok {
		l.pos++
		return Token{Kind: k, Text: string(c), Line: l.line}, nil
	}
	return Token{}, fmt.Errorf("line %d: unexpected character %q", l.line, string(c))
}

func (l *lexer) str(quote byte) (Token, error) {
	line := l.line
	l.pos++
	var sb strings.Builder
	for l.pos < len(l.src) {
		c := l.src[l.pos]
		if c == quote {
			l.pos++
			return Token{Kind: tString, Text: sb.String(), Line: line}, nil
		}
		if c == '\\' && l.pos+1 < len(l.src) {
			l.pos++
			switch l.src[l.pos] {
			case 'n':
				sb.WriteByte('\n')
			case 't':
				sb.WriteByte('\t')
			case 'r':
				sb.WriteByte('\r')
			default:
				sb.WriteByte(l.src[l.pos])
			}
			l.pos++
			continue
		}
		if c == '\n' {
			return Token{}, fmt.Errorf("line %d: unterminated string", line)
		}
		sb.WriteByte(c)
		l.pos++
	}
	return Token{}, fmt.Errorf("line %d: unterminated string", line)
}

func (l *lexer) path() (Token, error) {
	l.skip()
	if l.pos >= len(l.src) || l.src[l.pos] != '/' {
		return Token{}, fmt.Errorf("line %d: expected path starting with /", l.line)
	}
	start := l.pos
	for l.pos < len(l.src) {
		c := l.src[l.pos]
		if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
			break
		}
		l.pos++
	}
	return Token{Kind: tPath, Text: l.src[start:l.pos], Line: l.line}, nil
}

func isIdentStart(c byte) bool {
	return c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c == '_'
}

func isIdent(c byte) bool {
	return isIdentStart(c) || c >= '0' && c <= '9'
}
