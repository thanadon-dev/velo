package velo

import (
	"fmt"
	"strings"
)

const maxParams = 8

type Ctx struct {
	params  [maxParams]string
	nparams int
	Body    Value
	Method  string
	Path    string
}

func (c *Ctx) Param(i int) string {
	if i < 0 || i >= c.nparams {
		return ""
	}
	return c.params[i]
}

type Err struct {
	Status int
	Msg    string
}

func (e *Err) Error() string { return e.Msg }

var (
	errNotFound = &Err{404, "not found"}
	errBadBody  = &Err{400, "invalid body"}
)

type Expr func(*Ctx) (Value, *Err)

type Route struct {
	Method    string
	Pattern   string
	Params    []string
	Body      Expr
	Const     []byte
	ConstText bool
	Status    int
	UsesBody  bool
	Line      int
}

type Program struct {
	Routes []Route
	Store  *Store
}

var methods = map[string]bool{
	"GET": true, "POST": true, "PUT": true, "PATCH": true,
	"DELETE": true, "HEAD": true, "OPTIONS": true,
}

func Compile(src string, store *Store) (*Program, error) {
	if store == nil {
		store = NewStore()
	}
	p := &parser{lex: newLexer(src), store: store}
	if err := p.advance(); err != nil {
		return nil, err
	}
	prog := &Program{Store: store}
	for p.tok.Kind != tEOF {
		r, err := p.route()
		if err != nil {
			return nil, err
		}
		prog.Routes = append(prog.Routes, r)
	}
	if len(prog.Routes) == 0 {
		return nil, fmt.Errorf("no routes defined")
	}
	return prog, nil
}

type parser struct {
	lex    *lexer
	tok    Token
	store  *Store
	params []string
	pure   bool
	body   bool
}

func (p *parser) advance() error {
	t, err := p.lex.next()
	if err != nil {
		return err
	}
	p.tok = t
	return nil
}

func (p *parser) expect(k Kind) (Token, error) {
	if p.tok.Kind != k {
		return Token{}, fmt.Errorf("line %d: expected %s, got %q", p.tok.Line, kindNames[k], p.tok.Text)
	}
	t := p.tok
	return t, p.advance()
}

func (p *parser) route() (Route, error) {
	if p.tok.Kind != tIdent {
		return Route{}, fmt.Errorf("line %d: expected http method, got %q", p.tok.Line, p.tok.Text)
	}
	m := p.tok
	method := strings.ToUpper(m.Text)
	if !methods[method] {
		return Route{}, fmt.Errorf("line %d: unknown method %q", m.Line, m.Text)
	}
	pt, err := p.lex.path()
	if err != nil {
		return Route{}, err
	}
	if err := p.advance(); err != nil {
		return Route{}, err
	}
	if _, err := p.expect(tArrow); err != nil {
		return Route{}, err
	}
	params, err := patternParams(pt.Text, pt.Line)
	if err != nil {
		return Route{}, err
	}
	p.params, p.pure, p.body = params, true, false
	expr, err := p.expr()
	if err != nil {
		return Route{}, err
	}
	r := Route{
		Method:   method,
		Pattern:  pt.Text,
		Params:   params,
		Body:     expr,
		Status:   200,
		UsesBody: p.body,
		Line:     m.Line,
	}
	if method == "POST" {
		r.Status = 201
	}
	if p.pure {
		v, e := expr(nil)
		if e == nil {
			if s, ok := v.(string); ok {
				r.Const, r.ConstText = []byte(s), true
			} else {
				r.Const = AppendJSON(nil, v)
			}
		}
	}
	return r, nil
}

func patternParams(pattern string, line int) ([]string, error) {
	var params []string
	for _, seg := range strings.Split(strings.Trim(pattern, "/"), "/") {
		if strings.HasPrefix(seg, ":") {
			name := seg[1:]
			if name == "" {
				return nil, fmt.Errorf("line %d: empty parameter name in %q", line, pattern)
			}
			params = append(params, name)
		}
	}
	if len(params) > maxParams {
		return nil, fmt.Errorf("line %d: too many parameters (max %d)", line, maxParams)
	}
	return params, nil
}

func (p *parser) expr() (Expr, error) {
	switch p.tok.Kind {
	case tString:
		s := p.tok.Text
		return func(*Ctx) (Value, *Err) { return s, nil }, p.advance()
	case tNumber:
		n := p.tok.Num
		return func(*Ctx) (Value, *Err) { return n, nil }, p.advance()
	case tLBrace:
		return p.object()
	case tLBrack:
		return p.array()
	case tIdent:
		return p.chain()
	}
	return nil, fmt.Errorf("line %d: unexpected %q in expression", p.tok.Line, p.tok.Text)
}

func (p *parser) object() (Expr, error) {
	if err := p.advance(); err != nil {
		return nil, err
	}
	var keys []string
	var vals []Expr
	for p.tok.Kind != tRBrace {
		if p.tok.Kind != tIdent && p.tok.Kind != tString {
			return nil, fmt.Errorf("line %d: expected object key, got %q", p.tok.Line, p.tok.Text)
		}
		key := p.tok.Text
		if err := p.advance(); err != nil {
			return nil, err
		}
		if _, err := p.expect(tColon); err != nil {
			return nil, err
		}
		v, err := p.expr()
		if err != nil {
			return nil, err
		}
		keys = append(keys, key)
		vals = append(vals, v)
		if p.tok.Kind == tComma {
			if err := p.advance(); err != nil {
				return nil, err
			}
		}
	}
	if err := p.advance(); err != nil {
		return nil, err
	}
	return func(c *Ctx) (Value, *Err) {
		o := make(Object, len(keys))
		for i, v := range vals {
			got, err := v(c)
			if err != nil {
				return nil, err
			}
			o[i] = Field{keys[i], got}
		}
		return o, nil
	}, nil
}

func (p *parser) array() (Expr, error) {
	if err := p.advance(); err != nil {
		return nil, err
	}
	var items []Expr
	for p.tok.Kind != tRBrack {
		v, err := p.expr()
		if err != nil {
			return nil, err
		}
		items = append(items, v)
		if p.tok.Kind == tComma {
			if err := p.advance(); err != nil {
				return nil, err
			}
		}
	}
	if err := p.advance(); err != nil {
		return nil, err
	}
	return func(c *Ctx) (Value, *Err) {
		a := make(Array, len(items))
		for i, it := range items {
			got, err := it(c)
			if err != nil {
				return nil, err
			}
			a[i] = got
		}
		return a, nil
	}, nil
}

func (p *parser) chain() (Expr, error) {
	head := p.tok
	if err := p.advance(); err != nil {
		return nil, err
	}
	switch head.Text {
	case "true":
		return func(*Ctx) (Value, *Err) { return true, nil }, nil
	case "false":
		return func(*Ctx) (Value, *Err) { return false, nil }, nil
	case "null":
		return func(*Ctx) (Value, *Err) { return nil, nil }, nil
	case "db":
		p.pure = false
		return p.dbCall(head.Line)
	case "body":
		p.pure = false
		p.body = true
		return p.fields(func(c *Ctx) (Value, *Err) { return c.Body, nil })
	}
	if i := indexOf(p.params, head.Text); i >= 0 {
		p.pure = false
		return p.fields(func(c *Ctx) (Value, *Err) { return c.Param(i), nil })
	}
	return nil, fmt.Errorf("line %d: unknown identifier %q", head.Line, head.Text)
}

func (p *parser) fields(base Expr) (Expr, error) {
	cur := base
	for p.tok.Kind == tDot {
		if err := p.advance(); err != nil {
			return nil, err
		}
		name, err := p.expect(tIdent)
		if err != nil {
			return nil, err
		}
		prev, key := cur, name.Text
		cur = func(c *Ctx) (Value, *Err) {
			v, err := prev(c)
			if err != nil {
				return nil, err
			}
			o, ok := v.(Object)
			if !ok {
				return nil, nil
			}
			got, _ := o.Get(key)
			return got, nil
		}
	}
	return cur, nil
}

func (p *parser) dbCall(line int) (Expr, error) {
	if _, err := p.expect(tDot); err != nil {
		return nil, err
	}
	name, err := p.expect(tIdent)
	if err != nil {
		return nil, err
	}
	col := p.store.Collection(name.Text)
	if _, err := p.expect(tDot); err != nil {
		return nil, err
	}
	op, err := p.expect(tIdent)
	if err != nil {
		return nil, err
	}
	if _, err := p.expect(tLParen); err != nil {
		return nil, err
	}
	var args []Expr
	for p.tok.Kind != tRParen {
		a, err := p.expr()
		if err != nil {
			return nil, err
		}
		args = append(args, a)
		if p.tok.Kind == tComma {
			if err := p.advance(); err != nil {
				return nil, err
			}
		}
	}
	if err := p.advance(); err != nil {
		return nil, err
	}
	return dbOp(col, op.Text, args, line)
}

func dbOp(col *Collection, op string, args []Expr, line int) (Expr, error) {
	need := func(n int) error {
		if len(args) != n {
			return fmt.Errorf("line %d: db.%s.%s expects %d argument(s), got %d", line, col.Name, op, n, len(args))
		}
		return nil
	}
	switch op {
	case "all":
		if err := need(0); err != nil {
			return nil, err
		}
		return func(*Ctx) (Value, *Err) { return col.All(), nil }, nil
	case "count":
		if err := need(0); err != nil {
			return nil, err
		}
		return func(*Ctx) (Value, *Err) { return float64(col.Count()), nil }, nil
	case "find":
		if err := need(1); err != nil {
			return nil, err
		}
		a := args[0]
		return func(c *Ctx) (Value, *Err) {
			k, err := a(c)
			if err != nil {
				return nil, err
			}
			v, ok := col.Find(asKey(k))
			if !ok {
				return nil, errNotFound
			}
			return v, nil
		}, nil
	case "create":
		if err := need(1); err != nil {
			return nil, err
		}
		a := args[0]
		return func(c *Ctx) (Value, *Err) {
			v, err := a(c)
			if err != nil {
				return nil, err
			}
			if v == nil {
				return nil, errBadBody
			}
			return col.Create(v), nil
		}, nil
	case "update":
		if err := need(2); err != nil {
			return nil, err
		}
		ka, va := args[0], args[1]
		return func(c *Ctx) (Value, *Err) {
			k, err := ka(c)
			if err != nil {
				return nil, err
			}
			v, err := va(c)
			if err != nil {
				return nil, err
			}
			out, ok := col.Update(asKey(k), v)
			if !ok {
				return nil, errNotFound
			}
			return out, nil
		}, nil
	case "delete":
		if err := need(1); err != nil {
			return nil, err
		}
		a := args[0]
		return func(c *Ctx) (Value, *Err) {
			k, err := a(c)
			if err != nil {
				return nil, err
			}
			if !col.Delete(asKey(k)) {
				return nil, errNotFound
			}
			return Object{{"deleted", true}}, nil
		}, nil
	}
	return nil, fmt.Errorf("line %d: unknown operation db.%s.%s", line, col.Name, op)
}

func asKey(v Value) string {
	switch t := v.(type) {
	case string:
		return t
	case float64:
		return string(appendNumber(nil, t))
	case nil:
		return ""
	default:
		return string(AppendJSON(nil, t))
	}
}

func indexOf(ss []string, s string) int {
	for i := range ss {
		if ss[i] == s {
			return i
		}
	}
	return -1
}
