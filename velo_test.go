package velo

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

const sampleApp = `
GET /health => "ok"
GET /users => db.users.all()
GET /users/:id => db.users.find(id)
POST /users => db.users.create(body)
PUT /users/:id => db.users.update(id, body)
DELETE /users/:id => db.users.delete(id)
GET /stats => { users: db.users.count(), ok: true }
GET /teams/:tid/members/:mid => { team: tid, member: mid }
`

func newTestServer(t *testing.T, src string) *Server {
	t.Helper()
	prog, err := Compile(src, nil)
	if err != nil {
		t.Fatalf("compile: %v", err)
	}
	s, err := NewServer(prog)
	if err != nil {
		t.Fatalf("server: %v", err)
	}
	return s
}

func do(t *testing.T, s *Server, method, path, body string) (int, string) {
	t.Helper()
	var r *http.Request
	if body == "" {
		r = httptest.NewRequest(method, path, nil)
	} else {
		r = httptest.NewRequest(method, path, strings.NewReader(body))
	}
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	return w.Code, w.Body.String()
}

func TestRoutesEndToEnd(t *testing.T) {
	s := newTestServer(t, sampleApp)

	if code, body := do(t, s, "GET", "/health", ""); code != 200 || body != "ok" {
		t.Fatalf("health: %d %q", code, body)
	}
	if code, body := do(t, s, "GET", "/users", ""); code != 200 || body != "[]" {
		t.Fatalf("empty users: %d %q", code, body)
	}
	code, body := do(t, s, "POST", "/users", `{"name":"mark","age":30}`)
	if code != 201 || body != `{"id":1,"name":"mark","age":30}` {
		t.Fatalf("create: %d %q", code, body)
	}
	if code, body := do(t, s, "GET", "/users/1", ""); code != 200 || body != `{"id":1,"name":"mark","age":30}` {
		t.Fatalf("find: %d %q", code, body)
	}
	if code, _ := do(t, s, "GET", "/users/99", ""); code != 404 {
		t.Fatalf("missing user should 404, got %d", code)
	}
	if code, body := do(t, s, "PUT", "/users/1", `{"age":31}`); code != 200 || body != `{"id":1,"name":"mark","age":31}` {
		t.Fatalf("update: %d %q", code, body)
	}
	if code, body := do(t, s, "GET", "/stats", ""); code != 200 || body != `{"users":1,"ok":true}` {
		t.Fatalf("stats: %d %q", code, body)
	}
	if code, body := do(t, s, "GET", "/teams/a1/members/b2", ""); code != 200 || body != `{"team":"a1","member":"b2"}` {
		t.Fatalf("multi param: %d %q", code, body)
	}
	if code, body := do(t, s, "DELETE", "/users/1", ""); code != 200 || body != `{"deleted":true}` {
		t.Fatalf("delete: %d %q", code, body)
	}
	if code, body := do(t, s, "GET", "/users", ""); code != 200 || body != "[]" {
		t.Fatalf("after delete: %d %q", code, body)
	}
	if code, _ := do(t, s, "GET", "/nope", ""); code != 404 {
		t.Fatal("unknown path should 404")
	}
	if code, _ := do(t, s, "DELETE", "/health", ""); code != 405 {
		t.Fatal("known path wrong method should 405")
	}
	if code, _ := do(t, s, "POST", "/users", `{oops}`); code != 400 {
		t.Fatal("invalid json body should 400")
	}
}

func TestTrailingSlashAndRoot(t *testing.T) {
	s := newTestServer(t, "GET / => \"root\"\nGET /a => \"a\"\n")
	if code, body := do(t, s, "GET", "/", ""); code != 200 || body != "root" {
		t.Fatalf("root: %d %q", code, body)
	}
	if code, body := do(t, s, "GET", "/a/", ""); code != 200 || body != "a" {
		t.Fatalf("trailing slash: %d %q", code, body)
	}
}

func TestDeleteReindexes(t *testing.T) {
	s := newTestServer(t, sampleApp)
	for i := 0; i < 3; i++ {
		do(t, s, "POST", "/users", `{"n":`+string(rune('0'+i))+`}`)
	}
	do(t, s, "DELETE", "/users/1", "")
	if code, body := do(t, s, "GET", "/users/3", ""); code != 200 || !strings.Contains(body, `"id":3`) {
		t.Fatalf("id 3 after delete: %d %q", code, body)
	}
	if code, body := do(t, s, "GET", "/users/2", ""); code != 200 || !strings.Contains(body, `"id":2`) {
		t.Fatalf("id 2 after delete: %d %q", code, body)
	}
	if code, _ := do(t, s, "GET", "/users/1", ""); code != 404 {
		t.Fatal("deleted id should 404")
	}
}

func TestBodyFieldAccess(t *testing.T) {
	s := newTestServer(t, `POST /echo => body.name`)
	if code, body := do(t, s, "POST", "/echo", `{"name":"velo"}`); code != 201 || body != "velo" {
		t.Fatalf("body field: %d %q", code, body)
	}
}

func TestCompileErrors(t *testing.T) {
	bad := []string{
		`GET /users => db.users.nope()`,
		`GET /users => unknownIdent`,
		`FETCH /users => "x"`,
		`GET /users db.users.all()`,
		`GET /users => db.users.find()`,
		`GET /users/: => "x"`,
		``,
	}
	for _, src := range bad {
		if _, err := Compile(src, nil); err == nil {
			t.Fatalf("expected error for %q", src)
		}
	}
}

func TestDuplicateRoute(t *testing.T) {
	prog, err := Compile("GET /a => \"1\"\nGET /a => \"2\"\n", nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := NewServer(prog); err == nil {
		t.Fatal("expected duplicate route error")
	}
}

func TestConstFolding(t *testing.T) {
	prog, err := Compile("GET /health => \"ok\"\nGET /users => db.users.all()\n", nil)
	if err != nil {
		t.Fatal(err)
	}
	if prog.Routes[0].Const == nil || !prog.Routes[0].ConstText {
		t.Fatal("literal route should fold to a const")
	}
	if prog.Routes[1].Const != nil {
		t.Fatal("db route must not fold")
	}
}

func TestJSONRoundTrip(t *testing.T) {
	cases := []string{
		`{"a":1,"b":"x","c":true,"d":null,"e":[1,2,3],"f":{"g":"h"}}`,
		`[]`,
		`{}`,
		`{"esc":"line\nquote\"tab\tend"}`,
		`{"th":"สวัสดี"}`,
		`{"f":1.5,"neg":-3,"big":1000000}`,
	}
	for _, in := range cases {
		v, err := ParseJSON([]byte(in))
		if err != nil {
			t.Fatalf("parse %s: %v", in, err)
		}
		if out := string(AppendJSON(nil, v)); out != in {
			t.Fatalf("roundtrip\n in: %s\nout: %s", in, out)
		}
	}
	for _, in := range []string{`{`, `{"a"}`, `tru`, `[1,]x`, ``, `"unterminated`} {
		if _, err := ParseJSON([]byte(in)); err == nil {
			t.Fatalf("expected json error for %q", in)
		}
	}
}

func TestConcurrentReadWrite(t *testing.T) {
	s := newTestServer(t, sampleApp)
	done := make(chan struct{})
	for i := 0; i < 4; i++ {
		go func() {
			for j := 0; j < 200; j++ {
				do(t, s, "POST", "/users", `{"n":1}`)
			}
			done <- struct{}{}
		}()
		go func() {
			for j := 0; j < 200; j++ {
				do(t, s, "GET", "/users", "")
			}
			done <- struct{}{}
		}()
	}
	for i := 0; i < 8; i++ {
		<-done
	}
	if code, body := do(t, s, "GET", "/stats", ""); code != 200 || body != `{"users":800,"ok":true}` {
		t.Fatalf("stats after concurrency: %d %q", code, body)
	}
}

func benchServer(b *testing.B, src string) *Server {
	b.Helper()
	prog, err := Compile(src, nil)
	if err != nil {
		b.Fatal(err)
	}
	s, err := NewServer(prog)
	if err != nil {
		b.Fatal(err)
	}
	return s
}

func BenchmarkRouterStatic(b *testing.B) {
	s := benchServer(b, sampleApp)
	c := &Ctx{}
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		if s.Router.Lookup("GET", "/users", c) == nil {
			b.Fatal("miss")
		}
	}
}

func BenchmarkRouterParam(b *testing.B) {
	s := benchServer(b, sampleApp)
	c := &Ctx{}
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		if s.Router.Lookup("GET", "/teams/a1/members/b2", c) == nil {
			b.Fatal("miss")
		}
	}
}

func BenchmarkServeConst(b *testing.B) {
	s := benchServer(b, sampleApp)
	req := httptest.NewRequest("GET", "/health", nil)
	b.ReportAllocs()
	b.RunParallel(func(pb *testing.PB) {
		for pb.Next() {
			w := httptest.NewRecorder()
			s.ServeHTTP(w, req)
		}
	})
}

func BenchmarkServeFind(b *testing.B) {
	s := benchServer(b, sampleApp)
	w := httptest.NewRecorder()
	s.ServeHTTP(w, httptest.NewRequest("POST", "/users", strings.NewReader(`{"name":"mark"}`)))
	req := httptest.NewRequest("GET", "/users/1", nil)
	b.ReportAllocs()
	b.RunParallel(func(pb *testing.PB) {
		for pb.Next() {
			rec := httptest.NewRecorder()
			s.ServeHTTP(rec, req)
		}
	})
}

func BenchmarkJSONEncodeRow(b *testing.B) {
	row := Object{{"id", 1.0}, {"name", "mark"}, {"email", "mark@example.com"}, {"active", true}}
	buf := make([]byte, 0, 256)
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		buf = AppendJSON(buf[:0], row)
	}
}

func BenchmarkJSONParseRow(b *testing.B) {
	raw := []byte(`{"id":1,"name":"mark","email":"mark@example.com","active":true}`)
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		if _, err := ParseJSON(raw); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkCompile(b *testing.B) {
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		if _, err := Compile(sampleApp, nil); err != nil {
			b.Fatal(err)
		}
	}
}
