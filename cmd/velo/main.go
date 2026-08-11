package main

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/thanadon-dev/velo"
)

const version = "0.1.0"

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(1)
	}
	switch os.Args[1] {
	case "run":
		if err := run(os.Args[2:]); err != nil {
			fail(err)
		}
	case "check":
		if err := check(os.Args[2:]); err != nil {
			fail(err)
		}
	case "routes":
		if err := routes(os.Args[2:]); err != nil {
			fail(err)
		}
	case "version", "-v", "--version":
		fmt.Println("velo " + version)
	case "help", "-h", "--help":
		usage()
	default:
		usage()
		os.Exit(1)
	}
}

func usage() {
	fmt.Print(`velo ` + version + `

usage:
  velo run <file.velo> [addr]    start the API server (default addr :8080)
  velo check <file.velo>         compile only, report errors
  velo routes <file.velo>        list compiled routes
  velo version
`)
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, "velo: "+err.Error())
	os.Exit(1)
}

func load(args []string) (*velo.Program, error) {
	if len(args) == 0 {
		return nil, fmt.Errorf("missing file argument")
	}
	src, err := os.ReadFile(args[0])
	if err != nil {
		return nil, err
	}
	return velo.Compile(string(src), nil)
}

func run(args []string) error {
	prog, err := load(args)
	if err != nil {
		return err
	}
	addr := ":8080"
	if len(args) > 1 {
		addr = args[1]
	}
	if v := os.Getenv("VELO_ADDR"); v != "" && len(args) <= 1 {
		addr = v
	}
	srv, err := velo.NewServer(prog)
	if err != nil {
		return err
	}
	hs := srv.HTTPServer(addr)
	go func() {
		for _, r := range prog.Routes {
			fmt.Printf("%-7s %s\n", r.Method, r.Pattern)
		}
		fmt.Printf("velo %s listening on %s\n", version, addr)
	}()
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	errc := make(chan error, 1)
	go func() { errc <- hs.ListenAndServe() }()
	select {
	case err := <-errc:
		return err
	case <-stop:
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		return hs.Shutdown(ctx)
	}
}

func check(args []string) error {
	prog, err := load(args)
	if err != nil {
		return err
	}
	fmt.Printf("ok: %d routes\n", len(prog.Routes))
	return nil
}

func routes(args []string) error {
	prog, err := load(args)
	if err != nil {
		return err
	}
	for _, r := range prog.Routes {
		kind := "dynamic"
		if r.Const != nil {
			kind = "const"
		}
		fmt.Printf("%-7s %-24s %d %s\n", r.Method, r.Pattern, r.Status, kind)
	}
	return nil
}
