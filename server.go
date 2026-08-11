package velo

import (
	"io"
	"net/http"
	"sync"
	"time"
)

const maxBodyBytes = 1 << 20

type Server struct {
	Router *Router
	Store  *Store
	ctxs   sync.Pool
	bufs   sync.Pool
}

func NewServer(prog *Program) (*Server, error) {
	r := NewRouter()
	for i := range prog.Routes {
		if err := r.Add(&prog.Routes[i]); err != nil {
			return nil, err
		}
	}
	s := &Server{Router: r, Store: prog.Store}
	s.ctxs.New = func() any { return &Ctx{} }
	s.bufs.New = func() any { b := make([]byte, 0, 4096); return &b }
	return s, nil
}

func (s *Server) ServeHTTP(w http.ResponseWriter, req *http.Request) {
	c := s.ctxs.Get().(*Ctx)
	rt := s.Router.Lookup(req.Method, req.URL.Path, c)
	if rt == nil {
		s.ctxs.Put(c)
		if s.Router.Allows(req.URL.Path) {
			writeErr(w, &Err{405, "method not allowed"})
			return
		}
		writeErr(w, &Err{404, "not found"})
		return
	}
	c.Method, c.Path, c.Body = req.Method, req.URL.Path, nil
	if rt.UsesBody {
		raw, err := io.ReadAll(http.MaxBytesReader(w, req.Body, maxBodyBytes))
		if err != nil {
			s.ctxs.Put(c)
			writeErr(w, &Err{413, "body too large"})
			return
		}
		if len(raw) > 0 {
			v, jerr := ParseJSON(raw)
			if jerr != nil {
				s.ctxs.Put(c)
				writeErr(w, errBadBody)
				return
			}
			c.Body = v
		}
	}
	if rt.Const != nil {
		s.ctxs.Put(c)
		h := w.Header()
		if rt.ConstText {
			h["Content-Type"] = ctypeText
		} else {
			h["Content-Type"] = ctypeJSON
		}
		w.WriteHeader(rt.Status)
		w.Write(rt.Const)
		return
	}
	v, rerr := rt.Body(c)
	s.ctxs.Put(c)
	if rerr != nil {
		writeErr(w, rerr)
		return
	}
	bp := s.bufs.Get().(*[]byte)
	buf := (*bp)[:0]
	h := w.Header()
	if str, ok := v.(string); ok {
		h["Content-Type"] = ctypeText
		buf = append(buf, str...)
	} else {
		h["Content-Type"] = ctypeJSON
		buf = AppendJSON(buf, v)
	}
	w.WriteHeader(rt.Status)
	w.Write(buf)
	*bp = buf
	s.bufs.Put(bp)
}

var (
	ctypeJSON = []string{"application/json"}
	ctypeText = []string{"text/plain; charset=utf-8"}
)

func writeErr(w http.ResponseWriter, e *Err) {
	h := w.Header()
	h["Content-Type"] = ctypeJSON
	w.WriteHeader(e.Status)
	w.Write(AppendJSON(nil, Object{{"error", e.Msg}}))
}

func (s *Server) HTTPServer(addr string) *http.Server {
	return &http.Server{
		Addr:              addr,
		Handler:           s,
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       15 * time.Second,
		WriteTimeout:      15 * time.Second,
		IdleTimeout:       60 * time.Second,
	}
}

func Run(src, addr string) error {
	prog, err := Compile(src, nil)
	if err != nil {
		return err
	}
	s, err := NewServer(prog)
	if err != nil {
		return err
	}
	return s.HTTPServer(addr).ListenAndServe()
}
