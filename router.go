package velo

import (
	"fmt"
	"strings"
)

const nMethods = 7

func methodIndex(m string) int {
	switch m {
	case "GET":
		return 0
	case "POST":
		return 1
	case "PUT":
		return 2
	case "PATCH":
		return 3
	case "DELETE":
		return 4
	case "HEAD":
		return 5
	case "OPTIONS":
		return 6
	}
	return -1
}

type node struct {
	static map[string]*node
	param  *node
	route  *Route
}

type Router struct {
	exact [nMethods]map[string]*Route
	trees [nMethods]*node
}

func NewRouter() *Router {
	return &Router{}
}

func (r *Router) Add(rt *Route) error {
	mi := methodIndex(rt.Method)
	if mi < 0 {
		return fmt.Errorf("unsupported method %q", rt.Method)
	}
	path := normalize(rt.Pattern)
	if !strings.Contains(path, ":") {
		if r.exact[mi] == nil {
			r.exact[mi] = map[string]*Route{}
		}
		if _, dup := r.exact[mi][path]; dup {
			return fmt.Errorf("duplicate route %s %s", rt.Method, rt.Pattern)
		}
		r.exact[mi][path] = rt
		return nil
	}
	if r.trees[mi] == nil {
		r.trees[mi] = &node{}
	}
	cur := r.trees[mi]
	for _, seg := range segments(path) {
		if strings.HasPrefix(seg, ":") {
			if cur.param == nil {
				cur.param = &node{}
			}
			cur = cur.param
			continue
		}
		if cur.static == nil {
			cur.static = map[string]*node{}
		}
		nxt, ok := cur.static[seg]
		if !ok {
			nxt = &node{}
			cur.static[seg] = nxt
		}
		cur = nxt
	}
	if cur.route != nil {
		return fmt.Errorf("duplicate route %s %s", rt.Method, rt.Pattern)
	}
	cur.route = rt
	return nil
}

func (r *Router) Lookup(method, path string, c *Ctx) *Route {
	mi := methodIndex(method)
	if mi < 0 {
		return nil
	}
	return r.lookupIndex(mi, path, c)
}

func (r *Router) Allows(path string) bool {
	for i := 0; i < nMethods; i++ {
		if r.lookupIndex(i, path, nil) != nil {
			return true
		}
	}
	return false
}

func (r *Router) lookupIndex(mi int, path string, c *Ctx) *Route {
	p := normalize(path)
	if m := r.exact[mi]; m != nil {
		if rt, ok := m[p]; ok {
			if c != nil {
				c.nparams = 0
			}
			return rt
		}
	}
	cur := r.trees[mi]
	if cur == nil {
		return nil
	}
	n := 0
	rest := strings.TrimPrefix(p, "/")
	for len(rest) > 0 {
		var seg string
		if j := strings.IndexByte(rest, '/'); j >= 0 {
			seg, rest = rest[:j], rest[j+1:]
		} else {
			seg, rest = rest, ""
		}
		if nxt, ok := cur.static[seg]; ok {
			cur = nxt
			continue
		}
		if cur.param != nil && seg != "" {
			if n < maxParams && c != nil {
				c.params[n] = seg
			}
			n++
			cur = cur.param
			continue
		}
		return nil
	}
	if cur.route == nil {
		return nil
	}
	if c != nil {
		c.nparams = n
	}
	return cur.route
}

func normalize(p string) string {
	if len(p) > 1 && p[len(p)-1] == '/' {
		return p[:len(p)-1]
	}
	if p == "" {
		return "/"
	}
	return p
}

func segments(p string) []string {
	t := strings.Trim(p, "/")
	if t == "" {
		return nil
	}
	return strings.Split(t, "/")
}
