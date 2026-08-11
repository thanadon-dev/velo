package velo

import (
	"errors"
	"math"
	"strconv"
	"unicode/utf8"
)

type Value any

type Field struct {
	K string
	V Value
}

type Object []Field

type Array []Value

func (o Object) Get(k string) (Value, bool) {
	for i := range o {
		if o[i].K == k {
			return o[i].V, true
		}
	}
	return nil, false
}

func (o Object) Set(k string, v Value) Object {
	for i := range o {
		if o[i].K == k {
			o[i].V = v
			return o
		}
	}
	return append(o, Field{k, v})
}

func (o Object) Clone() Object {
	c := make(Object, len(o))
	copy(c, o)
	return c
}

const hexDigits = "0123456789abcdef"

func AppendJSON(b []byte, v Value) []byte {
	switch t := v.(type) {
	case nil:
		return append(b, "null"...)
	case bool:
		if t {
			return append(b, "true"...)
		}
		return append(b, "false"...)
	case string:
		return appendString(b, t)
	case float64:
		return appendNumber(b, t)
	case int:
		return strconv.AppendInt(b, int64(t), 10)
	case int64:
		return strconv.AppendInt(b, t, 10)
	case Object:
		b = append(b, '{')
		for i := range t {
			if i > 0 {
				b = append(b, ',')
			}
			b = appendString(b, t[i].K)
			b = append(b, ':')
			b = AppendJSON(b, t[i].V)
		}
		return append(b, '}')
	case Array:
		b = append(b, '[')
		for i := range t {
			if i > 0 {
				b = append(b, ',')
			}
			b = AppendJSON(b, t[i])
		}
		return append(b, ']')
	default:
		return append(b, "null"...)
	}
}

func appendNumber(b []byte, f float64) []byte {
	if math.IsNaN(f) || math.IsInf(f, 0) {
		return append(b, "null"...)
	}
	if f == math.Trunc(f) && math.Abs(f) < 1e15 {
		return strconv.AppendInt(b, int64(f), 10)
	}
	return strconv.AppendFloat(b, f, 'g', -1, 64)
}

func appendString(b []byte, s string) []byte {
	b = append(b, '"')
	start := 0
	for i := 0; i < len(s); {
		c := s[i]
		if c >= 0x20 && c != '"' && c != '\\' && c < utf8.RuneSelf {
			i++
			continue
		}
		if start < i {
			b = append(b, s[start:i]...)
		}
		if c >= utf8.RuneSelf {
			i++
			start = i - 1
			continue
		}
		switch c {
		case '"':
			b = append(b, '\\', '"')
		case '\\':
			b = append(b, '\\', '\\')
		case '\n':
			b = append(b, '\\', 'n')
		case '\r':
			b = append(b, '\\', 'r')
		case '\t':
			b = append(b, '\\', 't')
		default:
			b = append(b, '\\', 'u', '0', '0', hexDigits[c>>4], hexDigits[c&0xf])
		}
		i++
		start = i
	}
	if start < len(s) {
		b = append(b, s[start:]...)
	}
	return append(b, '"')
}

var errJSON = errors.New("invalid json")

func ParseJSON(b []byte) (Value, error) {
	p := jsonParser{b: b}
	p.ws()
	v, err := p.value()
	if err != nil {
		return nil, err
	}
	p.ws()
	if p.i != len(p.b) {
		return nil, errJSON
	}
	return v, nil
}

type jsonParser struct {
	b []byte
	i int
}

func (p *jsonParser) ws() {
	for p.i < len(p.b) {
		switch p.b[p.i] {
		case ' ', '\t', '\n', '\r':
			p.i++
		default:
			return
		}
	}
}

func (p *jsonParser) value() (Value, error) {
	if p.i >= len(p.b) {
		return nil, errJSON
	}
	switch c := p.b[p.i]; {
	case c == '{':
		return p.object()
	case c == '[':
		return p.array()
	case c == '"':
		return p.str()
	case c == 't':
		return true, p.lit("true")
	case c == 'f':
		return false, p.lit("false")
	case c == 'n':
		return nil, p.lit("null")
	default:
		return p.number()
	}
}

func (p *jsonParser) lit(s string) error {
	if p.i+len(s) > len(p.b) || string(p.b[p.i:p.i+len(s)]) != s {
		return errJSON
	}
	p.i += len(s)
	return nil
}

func (p *jsonParser) object() (Value, error) {
	p.i++
	o := Object{}
	p.ws()
	if p.i < len(p.b) && p.b[p.i] == '}' {
		p.i++
		return o, nil
	}
	for {
		p.ws()
		k, err := p.str()
		if err != nil {
			return nil, err
		}
		p.ws()
		if p.i >= len(p.b) || p.b[p.i] != ':' {
			return nil, errJSON
		}
		p.i++
		p.ws()
		v, err := p.value()
		if err != nil {
			return nil, err
		}
		o = append(o, Field{k.(string), v})
		p.ws()
		if p.i >= len(p.b) {
			return nil, errJSON
		}
		if p.b[p.i] == ',' {
			p.i++
			continue
		}
		if p.b[p.i] == '}' {
			p.i++
			return o, nil
		}
		return nil, errJSON
	}
}

func (p *jsonParser) array() (Value, error) {
	p.i++
	a := Array{}
	p.ws()
	if p.i < len(p.b) && p.b[p.i] == ']' {
		p.i++
		return a, nil
	}
	for {
		p.ws()
		v, err := p.value()
		if err != nil {
			return nil, err
		}
		a = append(a, v)
		p.ws()
		if p.i >= len(p.b) {
			return nil, errJSON
		}
		if p.b[p.i] == ',' {
			p.i++
			continue
		}
		if p.b[p.i] == ']' {
			p.i++
			return a, nil
		}
		return nil, errJSON
	}
}

func (p *jsonParser) str() (Value, error) {
	if p.i >= len(p.b) || p.b[p.i] != '"' {
		return nil, errJSON
	}
	p.i++
	start := p.i
	for p.i < len(p.b) {
		c := p.b[p.i]
		if c == '"' {
			s := string(p.b[start:p.i])
			p.i++
			return s, nil
		}
		if c == '\\' {
			return p.strSlow(start)
		}
		p.i++
	}
	return nil, errJSON
}

func (p *jsonParser) strSlow(start int) (Value, error) {
	buf := make([]byte, 0, len(p.b)-start)
	buf = append(buf, p.b[start:p.i]...)
	for p.i < len(p.b) {
		c := p.b[p.i]
		switch c {
		case '"':
			p.i++
			return string(buf), nil
		case '\\':
			p.i++
			if p.i >= len(p.b) {
				return nil, errJSON
			}
			switch p.b[p.i] {
			case '"':
				buf = append(buf, '"')
			case '\\':
				buf = append(buf, '\\')
			case '/':
				buf = append(buf, '/')
			case 'n':
				buf = append(buf, '\n')
			case 't':
				buf = append(buf, '\t')
			case 'r':
				buf = append(buf, '\r')
			case 'b':
				buf = append(buf, '\b')
			case 'f':
				buf = append(buf, '\f')
			case 'u':
				if p.i+4 >= len(p.b) {
					return nil, errJSON
				}
				n, err := strconv.ParseUint(string(p.b[p.i+1:p.i+5]), 16, 32)
				if err != nil {
					return nil, errJSON
				}
				p.i += 4
				buf = utf8.AppendRune(buf, rune(n))
			default:
				return nil, errJSON
			}
			p.i++
		default:
			buf = append(buf, c)
			p.i++
		}
	}
	return nil, errJSON
}

func (p *jsonParser) number() (Value, error) {
	start := p.i
	for p.i < len(p.b) {
		c := p.b[p.i]
		if (c >= '0' && c <= '9') || c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E' {
			p.i++
			continue
		}
		break
	}
	if start == p.i {
		return nil, errJSON
	}
	f, err := strconv.ParseFloat(string(p.b[start:p.i]), 64)
	if err != nil {
		return nil, errJSON
	}
	return f, nil
}
