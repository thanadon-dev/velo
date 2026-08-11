package main

import (
	"bufio"
	"flag"
	"fmt"
	"net"
	"net/url"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

func main() {
	conns := flag.Int("c", 50, "concurrent connections")
	dur := flag.Duration("d", 5*time.Second, "duration")
	method := flag.String("m", "GET", "http method")
	body := flag.String("b", "", "request body")
	warm := flag.Duration("w", 500*time.Millisecond, "warmup")
	flag.Parse()
	if flag.NArg() != 1 {
		fmt.Fprintln(os.Stderr, "usage: velobench [-c n] [-d 5s] [-m GET] [-b body] http://host:port/path")
		os.Exit(1)
	}
	u, err := url.Parse(flag.Arg(0))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	path := u.Path
	if path == "" {
		path = "/"
	}
	req := buildRequest(*method, path, u.Host, *body)

	run(u.Host, req, *conns, *warm)
	stats := run(u.Host, req, *conns, *dur)
	stats.print(*conns)
}

func buildRequest(method, path, host, body string) []byte {
	var sb strings.Builder
	sb.WriteString(method + " " + path + " HTTP/1.1\r\nHost: " + host + "\r\n")
	if body != "" {
		sb.WriteString("Content-Type: application/json\r\nContent-Length: " + strconv.Itoa(len(body)) + "\r\n")
	}
	sb.WriteString("\r\n")
	sb.WriteString(body)
	return []byte(sb.String())
}

type result struct {
	lat    []time.Duration
	ok     int64
	errs   int64
	bytes  int64
	elapse time.Duration
}

func run(addr string, req []byte, conns int, d time.Duration) *result {
	var mu sync.Mutex
	total := &result{}
	deadline := time.Now().Add(d)
	var wg sync.WaitGroup
	start := time.Now()
	for i := 0; i < conns; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			r := worker(addr, req, deadline)
			mu.Lock()
			total.lat = append(total.lat, r.lat...)
			total.ok += r.ok
			total.errs += r.errs
			total.bytes += r.bytes
			mu.Unlock()
		}()
	}
	wg.Wait()
	total.elapse = time.Since(start)
	return total
}

func worker(addr string, req []byte, deadline time.Time) *result {
	r := &result{lat: make([]time.Duration, 0, 4096)}
	c, err := net.Dial("tcp", addr)
	if err != nil {
		r.errs++
		return r
	}
	defer c.Close()
	if tc, ok := c.(*net.TCPConn); ok {
		tc.SetNoDelay(true)
	}
	br := bufio.NewReaderSize(c, 8192)
	for time.Now().Before(deadline) {
		t0 := time.Now()
		if _, err := c.Write(req); err != nil {
			r.errs++
			return r
		}
		n, err := readResponse(br)
		if err != nil {
			r.errs++
			return r
		}
		r.lat = append(r.lat, time.Since(t0))
		r.ok++
		r.bytes += int64(n)
	}
	return r
}

func readResponse(br *bufio.Reader) (int, error) {
	total := 0
	clen := -1
	chunked := false
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			return 0, err
		}
		total += len(line)
		if line == "\r\n" {
			break
		}
		lower := strings.ToLower(line)
		if strings.HasPrefix(lower, "content-length:") {
			clen, _ = strconv.Atoi(strings.TrimSpace(line[15:]))
		}
		if strings.HasPrefix(lower, "transfer-encoding:") && strings.Contains(lower, "chunked") {
			chunked = true
		}
	}
	if chunked {
		for {
			line, err := br.ReadString('\n')
			if err != nil {
				return 0, err
			}
			size, err := strconv.ParseInt(strings.TrimSpace(line), 16, 64)
			if err != nil {
				return 0, err
			}
			if size == 0 {
				br.ReadString('\n')
				return total, nil
			}
			if _, err := br.Discard(int(size) + 2); err != nil {
				return 0, err
			}
			total += int(size)
		}
	}
	if clen > 0 {
		if _, err := br.Discard(clen); err != nil {
			return 0, err
		}
		total += clen
	}
	return total, nil
}

func (r *result) print(conns int) {
	sort.Slice(r.lat, func(i, j int) bool { return r.lat[i] < r.lat[j] })
	rps := float64(r.ok) / r.elapse.Seconds()
	fmt.Printf("connections %d  duration %.1fs  requests %d  errors %d\n", conns, r.elapse.Seconds(), r.ok, r.errs)
	fmt.Printf("rps         %.0f\n", rps)
	fmt.Printf("throughput  %.1f MB/s\n", float64(r.bytes)/r.elapse.Seconds()/(1<<20))
	if len(r.lat) == 0 {
		return
	}
	fmt.Printf("latency     avg %s  p50 %s  p90 %s  p99 %s  max %s\n",
		round(avg(r.lat)), round(pct(r.lat, 0.50)), round(pct(r.lat, 0.90)),
		round(pct(r.lat, 0.99)), round(r.lat[len(r.lat)-1]))
}

func avg(l []time.Duration) time.Duration {
	var sum time.Duration
	for _, d := range l {
		sum += d
	}
	return sum / time.Duration(len(l))
}

func pct(l []time.Duration, p float64) time.Duration {
	i := int(float64(len(l)) * p)
	if i >= len(l) {
		i = len(l) - 1
	}
	return l[i]
}

func round(d time.Duration) time.Duration {
	if d < time.Millisecond {
		return d.Round(time.Microsecond)
	}
	return d.Round(10 * time.Microsecond)
}
