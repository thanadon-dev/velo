package velo

import (
	"strconv"
	"sync"
	"sync/atomic"
)

type snapshot struct {
	rows Array
	byID map[string]int
}

type Collection struct {
	Name   string
	mu     sync.Mutex
	snap   atomic.Pointer[snapshot]
	nextID int64
}

type Store struct {
	mu   sync.Mutex
	cols map[string]*Collection
}

func NewStore() *Store {
	return &Store{cols: map[string]*Collection{}}
}

func (s *Store) Collection(name string) *Collection {
	s.mu.Lock()
	defer s.mu.Unlock()
	if c, ok := s.cols[name]; ok {
		return c
	}
	c := &Collection{Name: name}
	c.snap.Store(&snapshot{rows: Array{}, byID: map[string]int{}})
	s.cols[name] = c
	return c
}

func (s *Store) Names() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]string, 0, len(s.cols))
	for n := range s.cols {
		out = append(out, n)
	}
	return out
}

func (c *Collection) All() Array {
	return c.snap.Load().rows
}

func (c *Collection) Count() int {
	return len(c.snap.Load().rows)
}

func (c *Collection) Find(id string) (Value, bool) {
	s := c.snap.Load()
	i, ok := s.byID[id]
	if !ok {
		return nil, false
	}
	return s.rows[i], true
}

func (c *Collection) Create(v Value) Value {
	c.mu.Lock()
	defer c.mu.Unlock()
	old := c.snap.Load()
	c.nextID++
	id := c.nextID
	idStr := strconv.FormatInt(id, 10)
	row := withID(v, float64(id))
	rows := make(Array, len(old.rows), len(old.rows)+1)
	copy(rows, old.rows)
	rows = append(rows, row)
	byID := make(map[string]int, len(old.byID)+1)
	for k, i := range old.byID {
		byID[k] = i
	}
	byID[idStr] = len(rows) - 1
	c.snap.Store(&snapshot{rows: rows, byID: byID})
	return row
}

func (c *Collection) Update(id string, v Value) (Value, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	old := c.snap.Load()
	i, ok := old.byID[id]
	if !ok {
		return nil, false
	}
	merged := merge(old.rows[i], v)
	rows := make(Array, len(old.rows))
	copy(rows, old.rows)
	rows[i] = merged
	c.snap.Store(&snapshot{rows: rows, byID: old.byID})
	return merged, true
}

func (c *Collection) Delete(id string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	old := c.snap.Load()
	i, ok := old.byID[id]
	if !ok {
		return false
	}
	rows := make(Array, 0, len(old.rows)-1)
	rows = append(rows, old.rows[:i]...)
	rows = append(rows, old.rows[i+1:]...)
	byID := make(map[string]int, len(old.byID)-1)
	for k, j := range old.byID {
		switch {
		case k == id:
		case j > i:
			byID[k] = j - 1
		default:
			byID[k] = j
		}
	}
	c.snap.Store(&snapshot{rows: rows, byID: byID})
	return true
}

func (c *Collection) Reset() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.nextID = 0
	c.snap.Store(&snapshot{rows: Array{}, byID: map[string]int{}})
}

func withID(v Value, id float64) Value {
	o, ok := v.(Object)
	if !ok {
		return Object{{"id", id}, {"value", v}}
	}
	row := make(Object, 0, len(o)+1)
	row = append(row, Field{"id", id})
	for i := range o {
		if o[i].K == "id" {
			continue
		}
		row = append(row, o[i])
	}
	return row
}

func merge(base, patch Value) Value {
	b, ok := base.(Object)
	if !ok {
		return patch
	}
	p, ok := patch.(Object)
	if !ok {
		return base
	}
	out := b.Clone()
	for i := range p {
		if p[i].K == "id" {
			continue
		}
		out = out.Set(p[i].K, p[i].V)
	}
	return out
}
