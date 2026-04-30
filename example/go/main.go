package main

import (
	"fmt"
	"log"
	"net/http"

	"github.com/stealthrocket/net/wasip1"
)

func main() {
	// Use WASI‑socket‑aware listener.
	listener, err := wasip1.Listen("tcp", "0.0.0.0:8080")
	if err != nil {
		log.Fatalf("failed to listen: %v", err)
	}

	// Single test path.
	http.HandleFunc("/ping", func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprintf(w, "pong from Go WASI TCP server\n")
	})

	log.Println("listening on http://0.0.0.0:8080/ping")
	if err := http.Serve(listener, nil); err != nil {
		log.Fatalf("http.Serve: %v", err)
	}
}
